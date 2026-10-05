use super::*;
use crate::retry::delay_ms;
use xcb_core::policy::{EffectState, Failure, Terminal, TurnFacts};

impl Store {
    pub(super) fn capture_recovery_generation(
        &self,
        db: &Connection,
        run: &RunRecord,
    ) -> Result<()> {
        let generation =
            crate::application_qualification::read_generation(&self.root, &run.account)?;
        db.execute(
            "INSERT INTO run_recovery_generation(run,generation) VALUES(?1,?2)",
            params![run.id.as_str(), generation],
        )?;
        Ok(())
    }

    fn account_recovery_from(
        &self,
        db: &Connection,
        account: &Id,
    ) -> Result<Option<AccountRecovery>> {
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='account_recovery')", [], |r| r.get(0))?;
        if !exists {
            return Ok(None);
        }
        let row: Option<(Option<String>, String)> = db
            .query_row(
                "SELECT generation,payload FROM account_recovery WHERE account=?1",
                [account.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((generation, payload)) = row else {
            return Ok(None);
        };
        if generation != crate::application_qualification::read_generation(&self.root, account)? {
            return Ok(None);
        }
        Ok(Some(decode(&payload)?))
    }
    pub fn account_recovery(&self, account: &Id) -> Result<Option<AccountRecovery>> {
        self.account_recovery_from(&*self.db()?, account)
    }
    pub fn account_recovery_available(
        &self,
        account: &Id,
        now: u64,
        active_runs: u32,
    ) -> Result<bool> {
        self.account_recovery_available_from(&*self.db()?, account, now, active_runs)
    }
    pub(super) fn account_recovery_available_from(
        &self,
        db: &Connection,
        account: &Id,
        now: u64,
        active_runs: u32,
    ) -> Result<bool> {
        Ok(self.account_recovery_from(db, account)?.is_none_or(|r| {
            r.consecutive_failures == 0 || (r.next_eligible_at_ms <= now && active_runs == 0)
        }))
    }
    pub(super) fn record_account_recovery(
        &self,
        db: &Connection,
        run: &RunRecord,
        facts: &TurnFacts,
        now: u64,
    ) -> Result<()> {
        if !facts.joined || facts.effects == EffectState::Uncertain || facts.pending_attention {
            return Ok(());
        }
        let captured: Option<Option<String>> = db
            .query_row(
                "SELECT generation FROM run_recovery_generation WHERE run=?1",
                [run.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let generation =
            crate::application_qualification::read_generation(&self.root, &run.account)?;
        if captured != Some(generation.clone()) {
            return Ok(());
        }
        let old = self.account_recovery_from(db, &run.account)?;
        // An older concurrent completion cannot clear a newer refusal.
        if old
            .as_ref()
            .is_some_and(|r| r.consecutive_failures > 0 && r.observed_at_ms >= run.created_at_ms)
        {
            return Ok(());
        }
        let next = if facts.terminal == Terminal::Failed
            && facts.failure == Some(Failure::ProviderUnavailable)
        {
            let failures = old
                .as_ref()
                .map_or(1, |r| r.consecutive_failures.saturating_add(1));
            AccountRecovery {
                consecutive_failures: failures,
                observed_at_ms: now,
                next_eligible_at_ms: now.saturating_add(delay_ms(run.account.as_str(), failures)),
                last_success_at_ms: old.and_then(|r| r.last_success_at_ms),
            }
        } else if facts.terminal == Terminal::Completed && facts.failure.is_none() {
            AccountRecovery {
                consecutive_failures: 0,
                observed_at_ms: now,
                next_eligible_at_ms: 0,
                last_success_at_ms: Some(now),
            }
        } else {
            return Ok(());
        };
        if generation
            != crate::application_qualification::read_generation(&self.root, &run.account)?
        {
            return Ok(());
        }
        db.execute("INSERT INTO account_recovery(account,generation,payload) VALUES(?1,?2,?3) ON CONFLICT(account) DO UPDATE SET generation=excluded.generation,payload=excluded.payload", params![run.account.as_str(), generation, serde_json::to_string(&next)?])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{
        Provider,
        models::{Mode, ModelChoice},
    };

    #[test]
    fn provider_recovery_is_shared_persistent_and_allows_one_trial() {
        let dir = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let state = crate::private::directory(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        crate::config::Config {
            max_runs_per_account: 4,
            ..Default::default()
        }
        .save(&state, None)
        .unwrap();
        let store = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Codex,
            id: Id::new("gpt-5").unwrap(),
            label: "fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let sessions: Vec<_> = (0..3)
            .map(|_| {
                store
                    .create_session(&account.id, model.clone(), &workspace, 2)
                    .unwrap()
            })
            .collect();
        let run = store
            .prepare_run(&sessions[0].id, sessions[0].revision, 3)
            .unwrap();
        let failure = TurnFacts {
            terminal: Terminal::Failed,
            joined: true,
            effects: EffectState::None,
            pending_attention: false,
            failure: Some(Failure::ProviderUnavailable),
        };
        store
            .record_account_recovery(&store.db().unwrap(), &run, &failure, 4)
            .unwrap();
        store.settle(&run, State::Failed, 5).unwrap();
        let mut same_tick = run.clone();
        same_tick.created_at_ms = 4;
        let mut concurrent_success = failure.clone();
        concurrent_success.terminal = Terminal::Completed;
        concurrent_success.failure = None;
        store
            .record_account_recovery(&store.db().unwrap(), &same_tick, &concurrent_success, 4)
            .unwrap();
        let first = store.account_recovery(&account.id).unwrap().unwrap();
        assert_eq!(first.consecutive_failures, 1);
        assert!(first.next_eligible_at_ms > 4);
        let reopened = Store::open(&state).unwrap();
        assert_eq!(
            reopened
                .account_recovery(&account.id)
                .unwrap()
                .unwrap()
                .next_eligible_at_ms,
            first.next_eligible_at_ms
        );
        assert!(
            store
                .prepare_run(
                    &sessions[1].id,
                    sessions[1].revision,
                    first.next_eligible_at_ms - 1
                )
                .is_err()
        );
        assert!(
            store
                .prepare_probe(
                    &account.id,
                    Some(model.clone()),
                    first.next_eligible_at_ms - 1
                )
                .is_err(),
            "application inference must not bypass account backoff"
        );
        let metadata = store.prepare_probe(&account.id, None, 6).unwrap();
        store.settle(&metadata, State::Idle, 7).unwrap();
        assert_eq!(
            store
                .account_recovery(&account.id)
                .unwrap()
                .unwrap()
                .consecutive_failures,
            1,
            "metadata must not prove inference health"
        );
        let application_trial = store
            .prepare_probe(&account.id, Some(model), first.next_eligible_at_ms)
            .unwrap();
        assert!(
            store
                .prepare_run(
                    &sessions[1].id,
                    sessions[1].revision,
                    first.next_eligible_at_ms
                )
                .is_err(),
            "application recovery trial must exclude other callers"
        );
        store
            .settle(&application_trial, State::Idle, first.next_eligible_at_ms)
            .unwrap();
        let trial = store
            .prepare_run(
                &sessions[1].id,
                sessions[1].revision,
                first.next_eligible_at_ms,
            )
            .unwrap();
        assert!(
            reopened
                .prepare_run(
                    &sessions[2].id,
                    sessions[2].revision,
                    first.next_eligible_at_ms
                )
                .is_err()
        );
        let mut success = failure.clone();
        success.terminal = Terminal::Completed;
        success.failure = None;
        store
            .record_account_recovery(
                &store.db().unwrap(),
                &trial,
                &success,
                first.next_eligible_at_ms + 1,
            )
            .unwrap();
        store
            .settle(&trial, State::Idle, first.next_eligible_at_ms + 2)
            .unwrap();
        let healthy = store.account_recovery(&account.id).unwrap().unwrap();
        assert_eq!(healthy.consecutive_failures, 0);
        assert_eq!(
            healthy.last_success_at_ms,
            Some(first.next_eligible_at_ms + 1)
        );
        assert!(
            store
                .account_recovery_available(&account.id, first.next_eligible_at_ms + 2, 3)
                .unwrap()
        );
        // A refusal from an older concurrent run still closes a healthy account.
        store
            .record_account_recovery(
                &store.db().unwrap(),
                &run,
                &failure,
                first.next_eligible_at_ms + 3,
            )
            .unwrap();
        let refused = store.account_recovery(&account.id).unwrap().unwrap();
        assert_eq!(refused.consecutive_failures, 1);
        // But an older concurrent success cannot clear that newer refusal.
        store
            .record_account_recovery(
                &store.db().unwrap(),
                &run,
                &success,
                first.next_eligible_at_ms + 4,
            )
            .unwrap();
        assert_eq!(
            store
                .account_recovery(&account.id)
                .unwrap()
                .unwrap()
                .observed_at_ms,
            refused.observed_at_ms
        );
        let db = store.db().unwrap();
        db.execute("DELETE FROM runs WHERE id=?1", [run.id.as_str()])
            .unwrap();
        let snapshots: u32 = db
            .query_row(
                "SELECT count(*) FROM run_recovery_generation WHERE run=?1",
                [run.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            snapshots, 0,
            "run retention must cascade recovery snapshots"
        );
    }

    #[test]
    fn provider_recovery_does_not_grade_unsettled_auth_policy_or_ambiguous_transport() {
        let dir = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(dir.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        for failure in [
            Failure::Transport,
            Failure::Authentication,
            Failure::Policy,
            Failure::Unknown,
        ] {
            let facts = TurnFacts {
                terminal: Terminal::Failed,
                joined: true,
                effects: EffectState::None,
                pending_attention: false,
                failure: Some(failure),
            };
            store
                .record_account_recovery(&store.db().unwrap(), &run, &facts, 3)
                .unwrap();
        }
        for (joined, effects, attention) in [
            (false, EffectState::None, false),
            (true, EffectState::Uncertain, false),
            (true, EffectState::None, true),
        ] {
            let facts = TurnFacts {
                terminal: Terminal::Failed,
                joined,
                effects,
                pending_attention: attention,
                failure: Some(Failure::ProviderUnavailable),
            };
            store
                .record_account_recovery(&store.db().unwrap(), &run, &facts, 3)
                .unwrap();
        }
        assert!(store.account_recovery(&account.id).unwrap().is_none());
        // Generation changes hide old availability evidence, never auth failures.
        let facts = TurnFacts {
            terminal: Terminal::Failed,
            joined: true,
            effects: EffectState::None,
            pending_attention: false,
            failure: Some(Failure::ProviderUnavailable),
        };
        store
            .record_account_recovery(&store.db().unwrap(), &run, &facts, 3)
            .unwrap();
        crate::application_qualification::ensure_generation(&store, &run).unwrap();
        assert!(store.account_recovery(&account.id).unwrap().is_none());
        store
            .record_account_recovery(&store.db().unwrap(), &run, &facts, 4)
            .unwrap();
        assert!(
            store.account_recovery(&account.id).unwrap().is_none(),
            "an old run must not grade a new credential generation"
        );
        store.settle(&run, State::Failed, 4).unwrap();
    }
}
