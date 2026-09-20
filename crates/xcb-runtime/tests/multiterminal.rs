//! Synthetic independent terminal owners: no provider process, credential or
//! account recovery is used. Existing storage tests cover parallel DB startup;
//! this test covers contention and remote UI behavior after startup.
use std::{
    collections::BTreeSet,
    fs,
    sync::{Arc, Barrier, mpsc::sync_channel},
    time::Duration,
};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
    session::{Message, Role, State},
    ui::{Intent, Update},
};
use xcb_runtime::{kernel, new_id, store::Store};

const TERMINALS: usize = 20;
const REMOTE_CANCEL: &str = "This turn is running in another terminal; cancel it there.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_terminals_share_account_custody_and_remote_resume_cannot_cancel_its_owner() {
    for provider in Provider::ALL {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let workspace = base.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let state = base.join("state");
        let inspector = Store::open(&state).unwrap();
        let account = inspector
            .add_account(provider, "Shared synthetic account", "Synthetic", 1)
            .unwrap();
        let model = ModelChoice {
            provider,
            id: Id::new("synthetic-model").unwrap(),
            label: "Synthetic model".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let mut terminals = Vec::new();
        for index in 0..TERMINALS {
            let store = Arc::new(Store::open(&state).unwrap());
            let session = store
                .create_session(&account.id, model.clone(), &workspace, 2)
                .unwrap();
            let message = Message {
                id: new_id("m"),
                role: Role::User,
                text: format!("Synthetic terminal {index}"),
                attachments: vec![],
                at_ms: 3,
                provenance: None,
            };
            let session = store
                .append_message(&session.id, session.revision, &message)
                .unwrap();
            terminals.push((store, session, message));
        }
        assert_eq!(
            terminals
                .iter()
                .map(|(store, _, _)| store.instance())
                .collect::<BTreeSet<_>>()
                .len(),
            TERMINALS,
            "each terminal must have independent durable ownership",
        );

        // Every contender has a current, distinct session revision. Any loser
        // must lose specifically to account custody, not a stale session CAS.
        let start = Arc::new(Barrier::new(TERMINALS));
        let workers: Vec<_> = terminals
            .into_iter()
            .map(|(store, session, message)| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let run = store.prepare_run(&session.id, session.revision, 4);
                    (store, session, message, run)
                })
            })
            .collect();
        let terminals: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let winners: Vec<_> = terminals
            .iter()
            .filter_map(|(_, _, _, run)| run.as_ref().ok())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "one live account lease across twenty terminals"
        );
        let winning_run = winners[0];
        let winning_session = winning_run.session.clone().unwrap();
        let before = inspector
            .recovery_candidate(&winning_run.id)
            .unwrap()
            .unwrap()
            .1;
        assert!(
            winning_run.pid.is_none(),
            "test must never launch a provider"
        );
        assert_eq!(inspector.unsettled_runs().unwrap().len(), 1);

        let mut viewers = Vec::new();
        for (store, session, message, attempted) in &terminals {
            let current = store.session(&session.id).unwrap().unwrap();
            assert_eq!(
                store.messages(&session.id, 10).unwrap()[0].text,
                message.text
            );
            if attempted.is_ok() {
                assert_eq!(current.revision, session.revision + 1);
                assert_eq!(current.state, State::Working);
                continue;
            }
            assert!(
                attempted
                    .as_ref()
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("account has an unsettled run")
            );
            assert_eq!(
                current.revision, session.revision,
                "rejected runs must not mutate sessions"
            );
            assert_eq!(current.state, State::Idle);
            assert!(
                store
                    .prepare_run(&session.id, session.revision, 1_000_000)
                    .is_err(),
                "elapsed time cannot release the winner"
            );
            assert!(store.remote_active(&winning_session).unwrap());
            let (commands, input) = sync_channel(8);
            let (output, updates) = sync_channel(16);
            let task = tokio::spawn(kernel::serve(
                store.clone(),
                workspace.clone(),
                Some(session.id.clone()),
                input,
                output,
            ));
            commands
                .send(Intent::Resume(winning_session.clone()))
                .unwrap();
            commands.send(Intent::Cancel).unwrap();
            commands.send(Intent::Quit).unwrap();
            viewers.push((commands, updates, task));
        }
        assert_eq!(viewers.len(), TERMINALS - 1);
        for (_commands, updates, task) in viewers {
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let mut remote = false;
            let mut explained = false;
            let mut stopped = false;
            for update in updates.try_iter() {
                match update {
                    Update::View(view)
                        if view
                            .session
                            .as_ref()
                            .is_some_and(|session| session.id == winning_session) =>
                    {
                        assert!(view.remote_active);
                        assert_eq!(view.state, State::Working);
                        assert!(
                            view.accounts
                                .iter()
                                .any(|row| row.id == account.id && row.busy)
                        );
                        remote = true;
                    }
                    Update::Notice(notice) if notice == REMOTE_CANCEL => explained = true,
                    Update::Stopped => stopped = true,
                    _ => (),
                }
            }
            assert!(
                remote && explained && stopped,
                "remote resume/cancel/quit must remain responsive and honest"
            );
        }
        assert_eq!(
            inspector
                .recovery_candidate(&winning_run.id)
                .unwrap()
                .unwrap()
                .1,
            before,
            "remote cancel and quit must not alter the owner's run or lease"
        );
        assert_eq!(inspector.unsettled_runs().unwrap().len(), 1);
        assert!(inspector.remove_session(&winning_session).is_err());
    }
}
