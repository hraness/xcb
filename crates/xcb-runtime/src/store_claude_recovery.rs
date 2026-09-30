use super::*;
use crate::auth::claude_recovery as auth;

#[derive(Debug)]
pub struct ClaudeAuthRecoveryInfo {
    pub generations: Vec<String>,
    pub reauthentication_required: bool,
}

type Candidate = (RunRecord, String, auth::Proof);

impl Store {
    /// The owning authentication supervisor has independently joined this
    /// helper. Its receipt and process marker must disappear together so a
    /// crash cannot leave an unrecoverable half-settled authentication step.
    pub(crate) fn finish_claude_auth_helper(
        &self,
        run: &RunRecord,
        call: &str,
        marker: &str,
    ) -> Result<()> {
        if !auth::auth_marker(marker) || !call.starts_with("xcb_auth_claude_") {
            return Err(Error::Conflict("Claude sign-in helper evidence mismatch"));
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut current, payload) = self.owned_run_from(&tx, run)?;
        let effect: Option<(String, String)> = tx.query_row(
            "SELECT operation,input_digest FROM tool_effects WHERE run=?1 AND call=?2 AND settled=0",
            params![run.id.as_str(), call],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (operation, input_digest) =
            effect.ok_or(Error::Conflict("Claude sign-in helper receipt is absent"))?;
        if operation != "host_auth_claude_oauth"
            || current.capability_processes.remove(marker).is_none()
        {
            return Err(Error::Conflict("Claude sign-in helper evidence mismatch"));
        }
        if tx.execute(
            "UPDATE runs SET payload=?1 WHERE id=?2 AND account=?3 AND payload=?4",
            params![serde_json::to_string(&current)?, run.id.as_str(), run.account.as_str(), payload],
        )? != 1
            || tx.execute(
                "UPDATE tool_effects SET settled=1 WHERE run=?1 AND call=?2 AND operation=?3 AND input_digest=?4 AND settled=0",
                params![run.id.as_str(), call, operation, input_digest],
            )? != 1
        {
            return Err(Error::Conflict("Claude sign-in helper authority changed"));
        }
        tx.commit()?;
        Ok(())
    }

    fn claude_auth_recovery_from(
        &self,
        tx: &Transaction<'_>,
        id: &Id,
        expected: &str,
    ) -> Result<Option<Candidate>> {
        let payload: String = tx
            .query_row(
                "SELECT payload FROM runs WHERE id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Unavailable("run not found"))?;
        let run: RunRecord = decode(&payload)?;
        run.validate()?;
        let mut query = tx.prepare("SELECT call,operation,input_digest FROM tool_effects WHERE run=?1 AND settled=0 ORDER BY call LIMIT 65")?;
        let effects = query
            .query_map([id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !effects
            .iter()
            .any(|(call, op, _)| auth::recognized(call, op))
            && !run
                .capability_processes
                .keys()
                .any(|name| auth::auth_marker(name))
        {
            return Ok(None);
        }
        if run.id != *id
            || digest(payload.as_bytes()) != expected
            || effects.is_empty()
            || effects.len() > 64
            || effects
                .iter()
                .any(|(call, op, _)| !auth::recognized(call, op))
            || run.command_custody.is_some()
            || run
                .capability_processes
                .keys()
                .any(|name| !auth::auth_marker(name))
        {
            return Err(Error::Conflict(
                "Claude sign-in recovery evidence is incomplete or changed",
            ));
        }
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE account=?1 AND run=?2)",
            params![run.account.as_str(), id.as_str()],
            |row| row.get(0),
        )?;
        if !held {
            return Err(Error::Conflict("run lease is absent"));
        }
        let account: Account = decode(&tx.query_row::<String, _, _>(
            "SELECT payload FROM accounts WHERE id=?1",
            [run.account.as_str()],
            |row| row.get(0),
        )?)?;
        if account.id != run.account || account.provider != Provider::Claude {
            return Err(Error::Conflict("Claude recovery account mismatch"));
        }
        if run.phase == "running" {
            run.verify_recovery_stop()?;
        } else if run.phase == "prepared" && run.pid.is_none() {
            // Auth runs before main launch and keeps its pending receipt until
            // credential verification finishes. These exact receipts plus all
            // recorded auth groups establish this narrowly scoped stop proof.
            let owner = run.owner.as_ref().ok_or(Error::CleanupUnproven)?;
            if owner.pid <= 1
                || i32::try_from(owner.pid).is_err()
                || crate::os::process_exists(owner.pid) != Some(false)
            {
                return Err(Error::Conflict(
                    "Claude sign-in owner has not been proven stopped",
                ));
            }
            for pid in run.capability_processes.values() {
                crate::process::prove_process_group_absent(pid.ok_or(Error::CleanupUnproven)?)?;
            }
        } else {
            return Err(Error::Conflict("Claude sign-in run cannot be recovered"));
        }
        let root =
            private::check_directory(&self.root.join("accounts").join(run.account.as_str()))?;
        let proof = auth::inspect(&root, &run, expected, &effects)?;
        Ok(Some((run, payload, proof)))
    }

    pub fn inspect_claude_auth_recovery(
        &self,
        id: &Id,
        expected: &str,
    ) -> Result<Option<ClaudeAuthRecoveryInfo>> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(self
            .claude_auth_recovery_from(&tx, id, expected)?
            .map(|(_, _, proof)| ClaudeAuthRecoveryInfo {
                generations: proof.generations,
                reauthentication_required: proof.reauthentication_required,
            }))
    }

    pub fn recover_claude_auth(&self, id: &Id, expected: &str, now: u64) -> Result<RunRecord> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (run, payload, proof) = self
            .claude_auth_recovery_from(&tx, id, expected)?
            .ok_or(Error::Conflict("run has no Claude sign-in to recover"))?;
        proof.retain()?;
        if proof.reauthentication_required {
            let generation =
                crate::application_qualification::read_generation(&self.root, &run.account)?;
            tx.execute("INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,?2,?3) ON CONFLICT(account) DO UPDATE SET generation=excluded.generation,run=excluded.run", params![run.account.as_str(), generation, run.id.as_str()])?;
        }
        if let Some(id) = &run.session {
            let mut session =
                session_from(&tx, id)?.ok_or(Error::Unavailable("session not found"))?;
            let revision = session.revision;
            session.revision = revision
                .checked_add(1)
                .ok_or(Error::Conflict("revision overflow"))?;
            session.state = State::Uncertain;
            session.last_active_at_ms = session.last_active_at_ms.max(now);
            update_session(&tx, &session, revision)?;
        }
        tx.execute(
            "UPDATE tool_effects SET settled=1 WHERE run=?1 AND settled=0",
            [id.as_str()],
        )?;
        let settled = RunRecord {
            phase: "settled".into(),
            capability_processes: BTreeMap::new(),
            ..run
        };
        if tx.execute(
            "UPDATE runs SET phase='settled',payload=?1 WHERE id=?2 AND account=?3 AND payload=?4",
            params![
                serde_json::to_string(&settled)?,
                id.as_str(),
                settled.account.as_str(),
                payload
            ],
        )? != 1
            || tx.execute(
                "DELETE FROM leases WHERE account=?1 AND run=?2",
                params![settled.account.as_str(), id.as_str()],
            )? != 1
        {
            return Err(Error::Conflict("Claude sign-in recovery authority changed"));
        }
        tx.commit()?;
        Ok(settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auth::fixtures::{self, ActiveGeneration, Evidence, STOPPED_PID};

    fn fixture() -> (tempfile::TempDir, Store, RunRecord) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(dir.path()).unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Recovery", 1, None)
            .unwrap();
        let run = store.prepare_probe(&account.id, None, 2).unwrap();
        (dir, store, run)
    }

    fn persist(store: &Store, run: &RunRecord) -> String {
        let payload = serde_json::to_string(run).unwrap();
        store
            .db()
            .unwrap()
            .execute(
                "UPDATE runs SET phase=?1,payload=?2 WHERE id=?3",
                params![run.phase, payload, run.id.as_str()],
            )
            .unwrap();
        digest(payload)
    }

    fn stopped(store: &Store, run: &RunRecord, running: bool) -> (RunRecord, String) {
        let mut run = store.run(&run.id).unwrap().unwrap();
        run.owner.as_mut().unwrap().pid = STOPPED_PID;
        if running {
            run.phase = "running".into();
            run.pid = Some(STOPPED_PID);
        }
        let digest = persist(store, &run);
        (run, digest)
    }

    fn live_group() -> u32 {
        #[cfg(unix)]
        {
            rustix::process::getpgrp().as_raw_nonzero().get() as u32
        }
        #[cfg(windows)]
        {
            std::process::id()
        }
    }

    fn snapshot(store: &Store) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let db = store.db().unwrap();
        [
            "accounts",
            "runs",
            "leases",
            "tool_effects",
            "account_auth_failures",
        ]
        .into_iter()
        .map(|table| {
            let mut query = db
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let count = query.column_count();
            query
                .query_map([], |row| (0..count).map(|index| row.get(index)).collect())
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect()
    }

    fn assert_refused_unchanged(store: &Store, run: &RunRecord, digest: &str, evidence: &Evidence) {
        let before = snapshot(store);
        assert!(store.inspect_claude_auth_recovery(&run.id, digest).is_err());
        assert!(store.recover_claude_auth(&run.id, digest, 10).is_err());
        assert_eq!(snapshot(store), before);
        assert!(!evidence.quarantine.exists());
    }

    #[test]
    fn claude_auth_recovery_stopped_matching_receipts_release_only_target_and_are_repeat_safe() {
        for (running, operation, active) in [
            (false, "login", ActiveGeneration::None),
            (true, "keychain-read", ActiveGeneration::Pending),
        ] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, active, operation);
            let other = store
                .add_account(Provider::Claude, "Other", 1, None)
                .unwrap();
            let other_run = store.prepare_probe(&other.id, None, 2).unwrap();
            store
                .begin_tool(
                    &other_run,
                    "foreign_pending",
                    "host_auth_import",
                    "synthetic",
                )
                .unwrap();
            let (run, digest) = stopped(&store, &run, running);
            let before = snapshot(&store);
            let info = store
                .inspect_claude_auth_recovery(&run.id, &digest)
                .unwrap()
                .unwrap();
            assert_eq!(info.generations, vec![evidence.generation.clone()]);
            assert!(!info.reauthentication_required);
            assert_eq!(snapshot(&store), before, "inspection must not mutate state");
            assert!(!evidence.quarantine.exists());

            let recovered = store.recover_claude_auth(&run.id, &digest, 10).unwrap();
            assert_eq!(recovered.phase, "settled");
            assert!(recovered.capability_processes.is_empty());
            assert_eq!(
                store
                    .unsettled_runs()
                    .unwrap()
                    .iter()
                    .map(|run| &run.id)
                    .collect::<Vec<_>>(),
                vec![&other_run.id]
            );
            assert_eq!(
                serde_json::to_string(&store.run(&other_run.id).unwrap().unwrap()).unwrap(),
                serde_json::to_string(&other_run).unwrap()
            );
            assert!(store.prepare_probe(&other.id, None, 11).is_err());
            assert!(!store.authentication_required(&run.account).unwrap());
            let settled: bool = store
                .db()
                .unwrap()
                .query_row(
                    "SELECT settled FROM tool_effects WHERE run=?1 AND call=?2",
                    params![run.id.as_str(), evidence.call],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(settled);
            evidence.assert_retained();
            evidence.assert_quarantined_without_secrets();

            let before = snapshot(&store);
            let quarantine = private::read(&evidence.quarantine, 64 * 1024).unwrap();
            assert!(store.recover_claude_auth(&run.id, &digest, 12).is_err());
            assert_eq!(snapshot(&store), before);
            assert_eq!(
                private::read(&evidence.quarantine, 64 * 1024).unwrap(),
                quarantine
            );
            evidence.assert_retained();
            assert!(store.prepare_probe(&run.account, None, 13).is_ok());
        }
    }

    #[test]
    fn claude_auth_recovery_active_mutation_requires_reauthentication_and_preserves_credentials() {
        for operation in ["login", "refresh", "publish"] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, ActiveGeneration::Pending, operation);
            let (run, digest) = stopped(&store, &run, false);
            let info = store
                .inspect_claude_auth_recovery(&run.id, &digest)
                .unwrap()
                .unwrap();
            assert!(info.reauthentication_required);
            assert!(!store.authentication_required(&run.account).unwrap());
            store.recover_claude_auth(&run.id, &digest, 10).unwrap();
            assert!(store.authentication_required(&run.account).unwrap());
            assert!(store.require_authenticated_account(&run.account).is_err());
            assert!(store.unsettled_runs().unwrap().is_empty());
            evidence.assert_retained();
            evidence.assert_quarantined_without_secrets();
        }
    }

    #[test]
    fn claude_auth_recovery_inactive_generation_preserves_previous_credentials_and_auth_state() {
        for prior_failure in [false, true] {
            for operation in ["login", "publish"] {
                let (_dir, store, run) = fixture();
                let evidence =
                    fixtures::evidence(&store, &run, ActiveGeneration::Previous, operation);
                if prior_failure {
                    store.db().unwrap().execute(
                        "INSERT INTO account_auth_failures(account,generation,run) VALUES(?1,NULL,'prior-run')",
                        [run.account.as_str()],
                    ).unwrap();
                }
                let (run, digest) = stopped(&store, &run, false);
                assert!(
                    !store
                        .inspect_claude_auth_recovery(&run.id, &digest)
                        .unwrap()
                        .unwrap()
                        .reauthentication_required
                );
                store.recover_claude_auth(&run.id, &digest, 10).unwrap();
                assert_eq!(
                    store.authentication_required(&run.account).unwrap(),
                    prior_failure
                );
                if prior_failure {
                    let failure_run: String = store
                        .db()
                        .unwrap()
                        .query_row(
                            "SELECT run FROM account_auth_failures WHERE account=?1",
                            [run.account.as_str()],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(failure_run, "prior-run");
                }
                evidence.assert_retained();
                evidence.assert_quarantined_without_secrets();
            }
        }
    }

    #[test]
    fn claude_auth_recovery_refuses_unproven_owner_or_process_and_changed_run_digest() {
        for case in [
            "live-owner",
            "missing-owner",
            "unknown-helper",
            "live-helper",
            "missing-provider",
            "changed-digest",
        ] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, ActiveGeneration::Previous, "login");
            let (mut run, original_digest) = stopped(&store, &run, case == "missing-provider");
            match case {
                "live-owner" => run.owner.as_mut().unwrap().pid = std::process::id(),
                "missing-owner" => run.owner = None,
                "unknown-helper" => *run.capability_processes.values_mut().next().unwrap() = None,
                "live-helper" => {
                    *run.capability_processes.values_mut().next().unwrap() = Some(live_group())
                }
                "missing-provider" => run.pid = None,
                "changed-digest" => run.created_at_ms += 1,
                _ => unreachable!(),
            }
            let current_digest = persist(&store, &run);
            let digest = if case == "changed-digest" {
                original_digest
            } else {
                current_digest
            };
            assert_refused_unchanged(&store, &run, &digest, &evidence);
            evidence.assert_retained();
        }
    }

    #[test]
    fn claude_auth_recovery_refuses_changed_receipts_and_mixed_foreign_effects() {
        for case in [
            "receipt-bytes",
            "receipt-digest",
            "foreign-effect",
            "foreign-marker",
            "missing-marker",
            "wrong-marker",
        ] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, ActiveGeneration::Previous, "login");
            if case == "foreign-effect" {
                store
                    .begin_tool(&run, "foreign_pending", "host_auth_import", "synthetic")
                    .unwrap();
            }
            let (mut run, _) = stopped(&store, &run, false);
            match case {
                "receipt-bytes" => {
                    let bytes = private::read(&evidence.intent, 64 * 1024).unwrap();
                    private::replace(&evidence.intent, b"{}", &digest(bytes)).unwrap();
                }
                "receipt-digest" => {
                    store
                        .db()
                        .unwrap()
                        .execute(
                            "UPDATE tool_effects SET input_digest=?1 WHERE run=?2 AND call=?3",
                            params!["a".repeat(64), run.id.as_str(), evidence.call],
                        )
                        .unwrap();
                }
                "foreign-marker" => {
                    run.capability_processes
                        .insert("browser".into(), Some(STOPPED_PID));
                }
                "missing-marker" => run.capability_processes.clear(),
                "wrong-marker" => {
                    run.capability_processes.clear();
                    run.capability_processes
                        .insert("claude_oauth_keychain".into(), Some(STOPPED_PID));
                }
                "foreign-effect" => (),
                _ => unreachable!(),
            }
            let digest = persist(&store, &run);
            assert_refused_unchanged(&store, &run, &digest, &evidence);
            if case == "receipt-bytes" {
                assert_eq!(private::read(&evidence.intent, 64 * 1024).unwrap(), b"{}");
            } else {
                evidence.assert_retained();
            }
        }
    }

    #[test]
    fn claude_auth_helper_finish_settles_only_the_receipt_and_exact_marker() {
        for (operation, marker) in [
            ("login", "claude_oauth_auth"),
            ("keychain-read", "claude_oauth_keychain"),
        ] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, ActiveGeneration::Previous, operation);
            store.mark_capability_starting(&run, "browser").unwrap();
            store
                .begin_tool(&run, "unrelated", "host_auth_import", "synthetic")
                .unwrap();
            store
                .finish_claude_auth_helper(&run, &evidence.call, marker)
                .unwrap();
            let current = store.run(&run.id).unwrap().unwrap();
            assert_eq!(
                current.capability_processes,
                BTreeMap::from([("browser".into(), None)])
            );
            let effects = store.db().unwrap().query_row(
                "SELECT (SELECT settled FROM tool_effects WHERE run=?1 AND call=?2),(SELECT settled FROM tool_effects WHERE run=?1 AND call='unrelated')",
                params![run.id.as_str(), evidence.call],
                |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
            ).unwrap();
            assert_eq!(effects, (true, false));
            assert_eq!(store.unsettled_runs().unwrap().len(), 1);
            assert!(store.prepare_probe(&run.account, None, 10).is_err());
            evidence.assert_retained();
            let before = snapshot(&store);
            assert!(
                store
                    .finish_claude_auth_helper(&run, &evidence.call, marker)
                    .is_err()
            );
            assert_eq!(snapshot(&store), before);
        }
    }

    #[test]
    fn claude_auth_helper_finish_rolls_back_missing_or_changed_evidence_and_write_failure() {
        for case in [
            "missing-receipt",
            "settled-receipt",
            "missing-marker",
            "foreign-operation",
            "foreign-owner",
            "write-failure",
        ] {
            let (_dir, store, run) = fixture();
            let evidence = fixtures::evidence(&store, &run, ActiveGeneration::Previous, "login");
            match case {
                "missing-receipt" => {
                    store
                        .db()
                        .unwrap()
                        .execute(
                            "DELETE FROM tool_effects WHERE run=?1 AND call=?2",
                            params![run.id.as_str(), evidence.call],
                        )
                        .unwrap();
                }
                "settled-receipt" => store.settle_tool(&run, &evidence.call).unwrap(),
                "missing-marker" => store
                    .clear_capability_custody(&run, "claude_oauth_auth")
                    .unwrap(),
                "foreign-operation" => {
                    store.db().unwrap().execute("UPDATE tool_effects SET operation='host_auth_import' WHERE run=?1 AND call=?2", params![run.id.as_str(), evidence.call]).unwrap();
                }
                "foreign-owner" => {
                    stopped(&store, &run, false);
                }
                "write-failure" => {
                    store.db().unwrap().execute_batch("CREATE TRIGGER reject_auth_settlement BEFORE UPDATE OF settled ON tool_effects BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(&store);
            assert!(
                store
                    .finish_claude_auth_helper(&run, &evidence.call, "claude_oauth_auth")
                    .is_err(),
                "{case}"
            );
            assert_eq!(snapshot(&store), before, "{case}");
            evidence.assert_retained();
        }
    }
}
