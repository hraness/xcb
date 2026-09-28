//! `algal.host-lifecycle.v1` projections of xcb account custody and the
//! committed `algal.host-profile.v1` record for the context-recipe host.
//!
//! These records are data-only projections. Building or reading one is a
//! pure read: no transaction, recovery, or admission path runs, no provider
//! is contacted, and nothing is retried, so the projection is also safe on
//! `Store::open_read_only`. An unsettled run whose owning process can no
//! longer be proven alive projects as `uncertain` with `pendingIntent` bound
//! to the exact stored run record — the same digest `xcb recover` requires —
//! never as `settled` or `failed`, and the account lease stays held.

use serde_json::{Value, json};

use super::*;

/// `usage.unit` in lifecycle records and `usageUnits.name` in the host
/// profile: the worst provider-reported share of a live subscription quota
/// window. Deliberately distinct from token counters and never API dollars.
const QUOTA_UNIT: &str = "subscription-quota";

/// The evaluator revision the context-recipe host pins; the fixture test
/// asserts it stays in lockstep with the `algal` dependency in
/// `crates/xcb-runtime/Cargo.toml`.
const ALGAL_EVALUATOR_REV: &str = "9922202a2da45bb1f7e0db82c0be5a1c6770c21b";

/// The contract's integer ceiling (`algal` rejects larger values).
const RECORD_INT_MAX: u64 = 4_294_967_295;

/// Canonical digest of `examples/context-recipes/coordination/replay.json`,
/// the committed offline replay evidence; the fixture test recomputes it.
const REPLAY_EVIDENCE: &str =
    "sha256:b026bbd49c3c559164c3f2bcd82a24e8ee532fb43562e3d9be0aee9d0181cf8b";

fn record_digest(value: &Value) -> Result<String> {
    algal::canonical::digest(value)
        .map_err(|_| xcb_core::Error::Invalid("host contract record").into())
}

/// The worst provider-reported share of a live account-scope window, using
/// the same window vocabulary and credential-generation binding as
/// `blocked_until_from`: a rotated or absent Claude credential projects no
/// measured share rather than another identity's meter, and windows that
/// cannot gate admission never feed the meter.
fn used_percent(db: &Connection, root: &Path, account: &Account, now: u64) -> Result<f64> {
    let windows = xcb_core::usage::account_windows(account.provider);
    if windows.is_empty() {
        return Ok(0.0);
    }
    let pool = if account.provider == Provider::Claude {
        match generation_pool(root, account)? {
            Some(pool) => pool,
            None => return Ok(0.0),
        }
    } else {
        account.quota_pool.clone()
    };
    if pool != account.quota_pool {
        return Ok(0.0);
    }
    let points = quotas_from(db, &pool)?;
    Ok(windows
        .iter()
        .filter_map(|window| {
            points
                .iter()
                .filter(|point| {
                    point.window.as_str() == *window
                        && point.validate().is_ok()
                        && point.observed_at_ms <= now
                        && now < point.resets_at_ms
                })
                .max_by_key(|point| point.observed_at_ms)
                .map(|point| point.used_percent)
        })
        .fold(0.0, f64::max))
}

impl Store {
    /// A read-only `algal.host-lifecycle.v1` projection of one account's
    /// custody slot. `owner` is the account's stable system-derived identity
    /// and `generation` counts its recorded custody acquisitions.
    ///
    /// `backlog.active` is at most one because the lease is exclusive;
    /// `backlog.queued` counts sessions still marked `working` that do not
    /// hold custody — xcb refuses a second turn at admission rather than
    /// queueing it, so the value is ordinarily zero. `pendingIntent` binds
    /// the exact stored run record (the digest `xcb recover` requires) and
    /// `receipt` binds the newest terminal outcome row. `usage` reports the
    /// worst live subscription-window share in `subscription-quota` units.
    /// A lease whose owner is no longer provably alive — or any unsettled
    /// record outside the single-lease invariant — projects as `uncertain`,
    /// never as `settled` or `failed`.
    pub fn host_lifecycle(&self, account: &Id, now: u64) -> Result<Value> {
        let db = self.db()?;
        let payload: String = db
            .query_row(
                "SELECT payload FROM accounts WHERE id=?1",
                [account.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::Unavailable("account not found"))?;
        let record: Account = decode(&payload)?;
        record.validate()?;
        if record.id != *account {
            return Err(Error::Conflict("account identity changed"));
        }
        // Custody is the lease row committed with the run. A second unsettled
        // run, or one without its lease, means custody is unproven: project
        // `uncertain` rather than silently picking one record.
        let mut unsettled: Vec<(String, RunRecord, bool)> = Vec::new();
        {
            let mut query = db.prepare(
                "SELECT r.payload, EXISTS(SELECT 1 FROM leases l WHERE l.run=r.id AND l.account=r.account)
                 FROM runs r WHERE r.account=?1 AND r.phase!='settled' ORDER BY r.id LIMIT 2",
            )?;
            let rows = query.query_map([account.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })?;
            for row in rows {
                let (payload, held) = row?;
                let run: RunRecord = decode(&payload)?;
                run.validate()?;
                if run.account != record.id || run.phase == "settled" {
                    return Err(Error::Conflict("run account or phase changed"));
                }
                unsettled.push((payload, run, held));
            }
        }
        let open = unsettled.first();
        let custody_proven = unsettled.len() == 1 && unsettled[0].2;

        let mut queued = 0_u64;
        {
            let mut query = db.prepare("SELECT payload FROM sessions WHERE account=?1 LIMIT ?2")?;
            let rows = query.query_map(params![account.as_str(), MAX_SESSIONS], |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let Ok(session) = decode::<Session>(&row?) else {
                    continue;
                };
                if session.validate().is_err()
                    || session.state != State::Working
                    || open.is_some_and(|(_, run, _)| run.session.as_ref() == Some(&session.id))
                {
                    continue;
                }
                queued += 1;
            }
        }

        let generation: u64 = db
            .query_row(
                "SELECT count(*) FROM runs WHERE account=?1",
                [account.as_str()],
                |row| row.get::<_, i64>(0),
            )?
            .try_into()
            .unwrap_or(0);

        let receipt = if db.query_row::<bool, _, _>(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='run_outcomes')",
            [],
            |row| row.get(0),
        )? {
            db.query_row(
                "SELECT o.payload FROM run_outcomes o JOIN runs r ON r.id=o.run
                 WHERE r.account=?1 ORDER BY o.rowid DESC LIMIT 1",
                [account.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| algal::canonical::digest_bytes(payload.as_bytes()))
        } else {
            None
        };

        let used = used_percent(&db, &self.root, &record, now)?;

        let mut held = vec!["subscription-account".to_owned()];
        let mut intent = None;
        // Custody takes precedence over the enabled flag: a lease can never
        // outlive a disable, but an unproven record projects `uncertain`
        // rather than `stopped` while custody is unclear.
        let (state, actions) = if let Some((payload, run, _)) = open {
            held.push("account-lease".into());
            if run.pid.is_some() {
                held.push("provider-process-group".into());
            }
            if run.command_custody.is_some() {
                held.push("guest-command".into());
            }
            intent = Some(algal::canonical::digest_bytes(payload.as_bytes()));
            if custody_proven && run.owner.as_ref().is_some_and(RunOwner::alive) {
                ("running", vec!["inspect", "stop"])
            } else {
                ("uncertain", vec!["inspect", "reconcile", "stop"])
            }
        } else if !record.enabled {
            ("stopped", vec!["inspect", "resume"])
        } else {
            ("ready", vec!["inspect", "stop"])
        };
        held.sort_unstable();
        held.dedup();

        let mut lifecycle = json!({
            "contract": algal::host_contract::HOST_LIFECYCLE_CONTRACT,
            "owner": record.fixed_name(),
            "generation": generation.min(RECORD_INT_MAX),
            "state": state,
            "pendingIntent": intent,
            "backlog": {
                "queued": queued.min(RECORD_INT_MAX),
                "active": u64::from(open.is_some()),
            },
            "heldAuthority": held,
            "usage": {"units": used, "charges": used, "unit": QUOTA_UNIT},
            "receipt": receipt,
            "permittedOperatorActions": actions,
        });
        lifecycle["digest"] = json!(record_digest(&lifecycle)?);
        algal::host_contract::parse_host_lifecycle(&lifecycle)
            .map_err(|_| xcb_core::Error::Invalid("host lifecycle projection"))?;
        Ok(lifecycle)
    }
}

/// The committed `algal.host-profile.v1` record for the context-recipe host:
/// `context_recipe` compiles a bounded `xcb.context-recipe.v1` program onto
/// the managed-program executor profile and dispatches its agent cells
/// through ordinary managed-task provider selection under the project's
/// grant.
///
/// Identity digests bind descriptors rather than copied state: the runtime
/// digest names the managed-program executor profile, the evaluator digest
/// the pinned `algal` revision, and the route digests the managed-task
/// admission route and its project-workspace scope. The record claims no
/// provider authority and grants none. The only `passed` probe carries the
/// canonical digest of the committed offline replay
/// (`examples/context-recipes/coordination/replay.json`); live provider
/// dispatch and custody reconciliation are `not-run`, never fabricated.
pub fn context_recipe_host_profile() -> Result<Value> {
    let mut profile = json!({
        "contract": algal::host_contract::HOST_PROFILE_CONTRACT,
        "host": {
            "id": "xcb-context-recipe",
            "kind": "subscription-cli",
            "version": "xcb.context-recipe.v1",
        },
        "runtime": {
            "runtimeDigest": record_digest(&json!({
                "contract": "xcb.managed-program-executor.v1",
                "profile": crate::managed_program::EXECUTOR_PROFILE,
            }))?,
            "evaluatorDigest": record_digest(&json!({
                "contract": "xcb.evaluator.v1",
                "evaluator": "algal",
                "rev": ALGAL_EVALUATOR_REV,
            }))?,
            "supportedContracts": [
                "algal.effect.v1",
                "algal.host-lifecycle.v1",
                "algal.host-profile.v1",
                "algal.organism.v1",
                "algal.run.v1",
                "xcb.context-recipe.v1",
            ],
        },
        "route": {
            "profileDigest": record_digest(&json!({
                "contract": "xcb.route-profile.v1",
                "profile": "xcb-managed-task",
                "providers": ["claude", "codex", "devin"],
                "selection": "project-grant",
            }))?,
            "scopeDigest": record_digest(&json!({
                "contract": "xcb.route-scope.v1",
                "scope": "project-workspace",
                "recipe": "xcb.context-recipe.v1",
            }))?,
            "accountScopeDigest": null,
        },
        "limits": {
            "maxConcurrent": 1,
            "maxQueue": crate::managed_program::MAX_MANAGED_CALLS,
            "maxInputBytes": crate::context_recipe::MAX_RECIPE_BYTES,
            "maxOutputBytes": crate::managed_program::MAX_SUMMARY_BYTES,
            "maxWork": crate::managed_program::MAX_MANAGED_CALLS,
        },
        "usageUnits": {
            "name": QUOTA_UNIT,
            "semantics": "Provider-reported share of a live subscription quota window consumed by the leased account; subscription allowance only, never API dollars or token counters.",
        },
        "resultRetention": {
            "mode": "original",
            "maxBytes": crate::context_recipe::MAX_RECIPE_BYTES,
            "originalRetrieval": true,
        },
        "uncertainEffectPolicy": "reconcile-required",
        "probes": [
            {
                "id": "live-provider-dispatch",
                "status": "not-run",
                "evidence": null,
            },
            {
                "id": "offline-replay",
                "status": "passed",
                "evidence": REPLAY_EVIDENCE,
            },
            {
                "id": "uncertain-custody-recovery",
                "status": "not-run",
                "evidence": null,
            },
        ],
        "absentCapabilities": [
            "api-dollar-metering",
            "credential-material",
            "host-messaging",
            "uncertain-auto-retry",
        ],
    });
    profile["digest"] = json!(record_digest(&profile)?);
    algal::host_contract::parse_host_profile(&profile)
        .map_err(|_| xcb_core::Error::Invalid("host profile record"))?;
    Ok(profile)
}

#[cfg(test)]
#[path = "host_contract_tests.rs"]
mod tests;
