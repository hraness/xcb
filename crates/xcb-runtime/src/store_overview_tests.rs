use super::*;
use xcb_core::{
    models::{Mode, ModelChoice},
    ui::STALE_ATTENTION_MS,
};

/// The fixtures' clock: every timestamp below is recent relative to it.
const NOW: u64 = 1_000;

struct Fixture {
    _root: tempfile::TempDir,
    store: Store,
    session: Session,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let store = Store::open(&base.join("state")).unwrap();
    let account = store
        .add_account(Provider::Codex, "Synthetic", 1, None)
        .unwrap();
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("fixture-model").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let session = store
        .create_session(&account.id, model, &workspace, 2)
        .unwrap();
    Fixture {
        _root: root,
        store,
        session,
    }
}

fn append(f: &Fixture, role: Role, text: &str, at_ms: u64) {
    let session = f.store.session(&f.session.id).unwrap().unwrap();
    f.store
        .append_message(
            &session.id,
            session.revision,
            &Message {
                id: new_id("m"),
                role,
                text: text.into(),
                at_ms,
                attachments: vec![],
                provenance: None,
            },
        )
        .unwrap();
}

#[test]
fn overview_direct_reads_only_latest_assistant_and_never_thinking_tool_or_user() {
    let f = fixture();
    assert!(f.store.agent_overview(None, NOW).unwrap()[0].response.is_empty());
    append(&f, Role::Assistant, "Previous answer", 3);
    append(&f, Role::Assistant, &"é".repeat(1400), 4);
    append(&f, Role::Thinking, "Private thinking", 5);
    append(&f, Role::Tool, "Tool output", 6);
    append(&f, Role::User, "New input", 7);
    let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
    assert_eq!(row.context, TranscriptContext::Session(f.session.id));
    assert_eq!(row.response, "é".repeat(1024));
    assert_eq!(row.model.as_deref(), Some("codex/fixture-model"));
    assert_eq!(row.updated_at_ms, 7);
}

#[test]
fn overview_direct_excludes_managed_worker_sessions_and_keeps_focused_within_limit() {
    let f = fixture();
    let mut attention = None;
    for index in 0..135 {
        let mut session = f.session.clone();
        session.id = new_id("s");
        session.last_active_at_ms += index + 1;
        if index == 0 {
            session.state = State::NeedsAnswer;
            attention = Some(session.id.clone());
        }
        if index == 134 {
            session.managed_task = Some(new_id("t"));
        }
        f.store
            .db()
            .unwrap()
            .execute(
                "INSERT INTO sessions(id,account,last_active,payload,revision) VALUES(?1,?2,?3,?4,0)",
                params![
                    session.id.as_str(),
                    session.account.as_str(),
                    session.last_active_at_ms as i64,
                    serde_json::to_string(&session).unwrap()
                ],
            )
            .unwrap();
    }
    let result = f.store.agent_overview(Some(&f.session.id), NOW).unwrap();
    assert_eq!(result.len(), MAX_AGENTS);
    assert_eq!(
        result[0].context,
        TranscriptContext::Session(attention.unwrap())
    );
    assert!(
        result
            .iter()
            .any(|row| row.context == TranscriptContext::Session(f.session.id.clone()))
    );
    assert!(result.iter().all(|row| row.updated_at_ms != 137));
}

#[test]
fn overview_direct_corrupt_response_identity_is_not_presented() {
    let f = fixture();
    append(&f, Role::Assistant, "Valid answer", 3);
    f.store
        .db()
        .unwrap()
        .execute(
            "UPDATE messages SET payload=json_set(payload,'$.id','other-message') WHERE session=?1",
            [f.session.id.as_str()],
        )
        .unwrap();
    assert!(f.store.agent_overview(None, NOW).unwrap()[0].response.is_empty());
}

#[test]
fn overview_direct_requires_live_owner_for_working_and_never_releases_custody() {
    let f = fixture();
    let current = f.store.session(&f.session.id).unwrap().unwrap();
    let mut run = f
        .store
        .prepare_run(&current.id, current.revision, 4)
        .unwrap();
    assert_eq!(
        f.store.agent_overview(None, NOW).unwrap()[0].state,
        State::Working
    );
    run.owner.as_mut().unwrap().pid = i32::MAX as u32;
    f.store
        .db()
        .unwrap()
        .execute(
            "UPDATE runs SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
        )
        .unwrap();
    let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
    assert_eq!(row.state, State::Uncertain);
    assert_eq!(row.activity, "needs recovery");
    assert_eq!(f.store.unsettled_runs().unwrap().len(), 1);
    assert_eq!(
        f.store.session(&f.session.id).unwrap().unwrap().state,
        State::Working
    );
}

#[test]
fn overview_direct_retains_previous_response_category_during_new_work() {
    use xcb_core::{
        policy::{EffectState, Terminal, TurnFacts},
        session::MessageProvenance,
    };
    for (previous_state, answer) in [
        (State::Idle, "Checks passed"),
        (State::NeedsAnswer, "Which version?"),
    ] {
        let f = fixture();
        append(&f, Role::User, "Inspect the problem", 3);
        let input = f.store.messages(&f.session.id, 1).unwrap().remove(0).id;
        let current = f.store.session(&f.session.id).unwrap().unwrap();
        let run = f
            .store
            .prepare_run(&current.id, current.revision, 4)
            .unwrap();
        let response = Message {
            id: new_id("m"),
            role: Role::Assistant,
            text: answer.into(),
            at_ms: 5,
            attachments: vec![],
            provenance: Some(MessageProvenance {
                account: f.session.account.clone(),
                model: f.session.model.clone(),
                run: Some(run.id.clone()),
            }),
        };
        let current = f.store.session(&f.session.id).unwrap().unwrap();
        f.store
            .append_message(&current.id, current.revision, &response)
            .unwrap();
        let outcome = crate::runner::Outcome {
            text: response.text,
            state: previous_state,
            tool_calls: Some(0),
            diagnostic: None,
            facts: TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: EffectState::None,
                pending_attention: previous_state.attention(),
                failure: None,
            },
        };
        f.store.settle_outcome(&run, &input, &outcome, 6).unwrap();
        assert_eq!(
            f.store.agent_overview(None, NOW).unwrap()[0].category.as_deref(),
            Some(previous_state.label())
        );
        append(&f, Role::User, "Version two", 7);
        let current = f.store.session(&f.session.id).unwrap().unwrap();
        f.store
            .prepare_run(&current.id, current.revision, 8)
            .unwrap();
        let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
        assert_eq!(row.state, State::Working);
        assert_eq!(row.category.as_deref(), Some(previous_state.label()));
        assert_eq!(row.response, answer);
    }
}

#[test]
fn overview_direct_failed_session_without_response_shows_the_diagnostic() {
    use xcb_core::policy::{EffectState, Terminal, TurnFacts};
    let f = fixture();
    append(&f, Role::User, "Add a test", 3);
    let input = f.store.messages(&f.session.id, 1).unwrap().remove(0).id;
    let current = f.store.session(&f.session.id).unwrap().unwrap();
    let run = f
        .store
        .prepare_run(&current.id, current.revision, 4)
        .unwrap();
    let diagnostic = "provider protocol error: effective runtime boundary mismatch: plugins";
    let outcome = crate::runner::Outcome {
        text: String::new(),
        state: State::Failed,
        tool_calls: Some(0),
        diagnostic: Some(crate::runner::Diagnostic::notice(diagnostic)),
        facts: TurnFacts {
            terminal: Terminal::Failed,
            joined: true,
            effects: EffectState::None,
            pending_attention: false,
            failure: None,
        },
    };
    f.store.settle_outcome(&run, &input, &outcome, 5).unwrap();
    let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
    assert_eq!(row.state, State::Failed);
    assert_eq!(row.response, diagnostic);
    assert_eq!(row.category.as_deref(), Some("failed"));
    // A retained answer still wins over the diagnostic.
    append(&f, Role::Assistant, "Recovered", 6);
    let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
    assert_eq!(row.response, "Recovered");
}

#[test]
fn overview_direct_legacy_store_without_outcome_table_keeps_response() {
    let f = fixture();
    append(&f, Role::Assistant, "Retained answer", 3);
    f.store
        .db()
        .unwrap()
        .execute_batch("DROP TABLE run_outcomes")
        .unwrap();
    let row = f.store.agent_overview(None, NOW).unwrap().remove(0);
    assert_eq!(row.response, "Retained answer");
    assert!(row.category.is_none());
}

#[test]
fn overview_direct_global_attention_failures_and_limits_survive_current_workspace_volume() {
    let f = fixture();
    let mut attention = Vec::new();
    let states = [State::NeedsAnswer, State::Failed, State::Limited];
    for index in 0..135 {
        let mut session = f.session.clone();
        session.id = new_id("s");
        session.last_active_at_ms += index + 1;
        if let Some(state) = states.get(index as usize) {
            session.workspace = "/another/workspace".into();
            session.state = *state;
            attention.push(session.id.clone());
        }
        f.store.db().unwrap().execute("INSERT INTO sessions(id,account,last_active,payload,revision) VALUES(?1,?2,?3,?4,0)", params![session.id.as_str(), session.account.as_str(), session.last_active_at_ms as i64, serde_json::to_string(&session).unwrap()]).unwrap();
    }
    let result = f.store.agent_overview(Some(&f.session.id), NOW).unwrap();
    assert_eq!(result.len(), MAX_AGENTS);
    for (row, (id, state)) in result.iter().zip(attention.iter().zip(states).rev()) {
        assert_eq!(row.context, TranscriptContext::Session(id.clone()));
        assert_eq!(row.state, state);
    }
    assert!(
        result
            .iter()
            .any(|row| row.context == TranscriptContext::Session(f.session.id.clone()))
    );
}

#[test]
fn overview_direct_stale_attention_cannot_displace_running_work_from_the_limit() {
    let f = fixture();
    let current = f.store.session(&f.session.id).unwrap().unwrap();
    f.store
        .prepare_run(&current.id, current.revision, 4)
        .unwrap();
    for index in 0..135 {
        let mut session = f.session.clone();
        session.id = new_id("s");
        session.state = State::Failed;
        session.last_active_at_ms = 10 + index;
        f.store.db().unwrap().execute("INSERT INTO sessions(id,account,last_active,payload,revision) VALUES(?1,?2,?3,?4,0)", params![session.id.as_str(), session.account.as_str(), session.last_active_at_ms as i64, serde_json::to_string(&session).unwrap()]).unwrap();
    }
    // Days later the failures are stale; the running session still makes the cut.
    let later = STALE_ATTENTION_MS + 10_000;
    let result = f.store.agent_overview(None, later).unwrap();
    assert_eq!(result.len(), MAX_AGENTS);
    assert_eq!(result[0].context, TranscriptContext::Session(f.session.id.clone()));
    assert_eq!(result[0].state, State::Working);
    assert!(result[1..].iter().all(|row| row.stale_attention(later)));
    // While the failures are recent they lead, and the limit keeps them.
    let result = f.store.agent_overview(None, NOW).unwrap();
    assert!(result.iter().all(|row| row.state == State::Failed));
}
