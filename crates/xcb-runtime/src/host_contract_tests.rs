use super::*;
use std::path::PathBuf;
use xcb_core::{
    models::{Mode, ModelChoice},
    session::State,
    usage::QuotaPoint,
};

const NOW: u64 = 1_000_000;
const FIXTURE: &str = include_str!("../tests/fixtures/context-recipe-host-profile.json");
const REPLAY: &str = include_str!("../../../examples/context-recipes/coordination/replay.json");

struct Fixture {
    _dir: tempfile::TempDir,
    state: PathBuf,
    store: Store,
    account: Account,
    session: Session,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(dir.path()).unwrap();
    let workspace = private::directory(&base.join("work")).unwrap();
    let state = base.join("state");
    let store = Store::open(&state).unwrap();
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
        _dir: dir,
        state,
        store,
        account,
        session,
    }
}

/// Replaces the run's recorded owner with a provably absent pid — the same
/// state `store::tests::orphaned` builds for recovery tests.
fn orphaned(store: &Store, run: &RunRecord) -> RunRecord {
    let mut run = run.clone();
    run.owner.as_mut().unwrap().pid = i32::MAX as u32;
    store
        .db()
        .unwrap()
        .execute(
            "UPDATE runs SET payload=?1 WHERE id=?2",
            params![serde_json::to_string(&run).unwrap(), run.id.as_str()],
        )
        .unwrap();
    run
}

fn intent(store: &Store, run: &Id) -> String {
    let (_, run_digest) = store.recovery_candidate(run).unwrap().unwrap();
    format!("sha256:{run_digest}")
}

/// Every mutable row, as stored, so a projection can prove it wrote nothing.
fn snapshot(store: &Store) -> Vec<(String, String)> {
    let db = store.db().unwrap();
    let mut rows = Vec::new();
    for select in [
        "SELECT id, payload FROM accounts ORDER BY id",
        "SELECT id, payload FROM sessions ORDER BY id",
        "SELECT id, payload FROM runs ORDER BY id",
        "SELECT account, run FROM leases ORDER BY account",
        "SELECT pool || '|' || window || '|' || observed_at, payload FROM quotas ORDER BY 1",
        "SELECT run, payload FROM run_outcomes ORDER BY run",
    ] {
        let mut query = db.prepare(select).unwrap();
        let listed = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        for row in listed {
            rows.push(row.unwrap());
        }
    }
    rows
}

#[test]
fn ready_slot_projects_a_valid_signed_record() {
    let f = fixture();
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    assert_eq!(record["contract"], "algal.host-lifecycle.v1");
    assert_eq!(record["owner"], f.account.fixed_name());
    assert_eq!(record["generation"], 0);
    assert_eq!(record["state"], "ready");
    assert_eq!(record["pendingIntent"], Value::Null);
    assert_eq!(record["backlog"], json!({"queued": 0, "active": 0}));
    assert_eq!(record["heldAuthority"], json!(["subscription-account"]));
    assert_eq!(
        record["usage"],
        json!({"units": 0.0, "charges": 0.0, "unit": "subscription-quota"})
    );
    assert_eq!(record["receipt"], Value::Null);
    assert_eq!(
        record["permittedOperatorActions"],
        json!(["inspect", "stop"])
    );
}

#[test]
fn leased_running_slot_projects_held_custody_and_pending_intent() {
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    let started = f.store.mark_spawned(&run, 42).unwrap();
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    assert_eq!(record["state"], "running");
    assert_eq!(
        record["pendingIntent"],
        json!(intent(&f.store, &started.id))
    );
    assert_eq!(record["generation"], 1);
    assert_eq!(record["backlog"], json!({"queued": 0, "active": 1}));
    assert_eq!(
        record["heldAuthority"],
        json!([
            "account-lease",
            "provider-process-group",
            "subscription-account"
        ])
    );
    assert_eq!(
        record["permittedOperatorActions"],
        json!(["inspect", "stop"])
    );
}

#[test]
fn unproven_lease_projects_uncertain_and_retains_pending_intent() {
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    let started = f.store.mark_spawned(&run, 42).unwrap();
    orphaned(&f.store, &started);
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    // The uncertain provider effect is reconciled, never retried: custody
    // stays held and the pending intent keeps binding the exact run record.
    assert_eq!(record["state"], "uncertain");
    assert_ne!(record["state"], json!("failed"));
    assert_ne!(record["state"], json!("settled"));
    assert_eq!(
        record["pendingIntent"],
        json!(intent(&f.store, &started.id))
    );
    assert_eq!(
        record["permittedOperatorActions"],
        json!(["inspect", "reconcile", "stop"])
    );
    assert_eq!(f.store.unsettled_runs().unwrap().len(), 1);

    // A prepared run never spawned is just as unproven once its owner dies.
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    orphaned(&f.store, &run);
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    assert_eq!(record["state"], "uncertain");
    assert_eq!(record["pendingIntent"], json!(intent(&f.store, &run.id)));
}

#[test]
fn disabled_slot_projects_stopped_and_a_settled_run_binds_a_receipt() {
    let f = fixture();
    f.store.set_account_enabled(&f.account.id, false).unwrap();
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    assert_eq!(record["state"], "stopped");
    assert_eq!(record["pendingIntent"], Value::Null);
    assert_eq!(
        record["permittedOperatorActions"],
        json!(["inspect", "resume"])
    );

    // A settled turn leaves custody free and `ready` again; the newest
    // terminal outcome binds as the receipt.
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    let started = f.store.mark_spawned(&run, 42).unwrap();
    f.store.settle(&started, State::Idle, 4).unwrap();
    let outcome = json!({"version": 1, "run": started.id.as_str()});
    {
        let db = f.store.db().unwrap();
        db.execute(
            "INSERT INTO run_outcomes(run,session,input_sequence,payload) VALUES(?1,?2,0,?3)",
            params![
                started.id.as_str(),
                f.session.id.as_str(),
                serde_json::to_string(&outcome).unwrap()
            ],
        )
        .unwrap();
    }
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    assert_eq!(record["state"], "ready");
    assert_eq!(record["backlog"], json!({"queued": 0, "active": 0}));
    assert_eq!(
        record["receipt"],
        json!(algal::canonical::digest_bytes(
            serde_json::to_string(&outcome).unwrap().as_bytes()
        ))
    );
    assert_eq!(record["generation"], 1);
}

#[test]
fn usage_reports_the_live_subscription_window_share_never_dollars() {
    let f = fixture();
    f.store
        .record_quota(&QuotaPoint {
            pool: f.account.quota_pool.clone(),
            window: Id::new("codex.primary").unwrap(),
            used_percent: 41.5,
            resets_at_ms: NOW + 60_000,
            observed_at_ms: NOW - 1_000,
        })
        .unwrap();
    // A window outside the account-scope vocabulary never feeds the meter,
    // and a window past its reset does not count as consumed.
    f.store
        .record_quota(&QuotaPoint {
            pool: f.account.quota_pool.clone(),
            window: Id::new("codex.model").unwrap(),
            used_percent: 99.0,
            resets_at_ms: NOW + 60_000,
            observed_at_ms: NOW - 1_000,
        })
        .unwrap();
    f.store
        .record_quota(&QuotaPoint {
            pool: f.account.quota_pool.clone(),
            window: Id::new("codex.secondary").unwrap(),
            used_percent: 88.0,
            resets_at_ms: NOW - 1,
            observed_at_ms: NOW - 120_000,
        })
        .unwrap();
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    algal::host_contract::parse_host_lifecycle(&record).unwrap();
    assert_eq!(
        record["usage"],
        json!({"units": 41.5, "charges": 41.5, "unit": "subscription-quota"})
    );
}

#[test]
fn projection_never_mutates_state_and_reads_through_open_read_only() {
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    let started = f.store.mark_spawned(&run, 42).unwrap();
    orphaned(&f.store, &started);
    let before = snapshot(&f.store);
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();
    assert_eq!(before, snapshot(&f.store));

    // `open_read_only` pins `query_only`: any write inside the projection
    // would fail, so an identical record is itself the non-mutation proof.
    let reader = Store::open_read_only(&f.state).unwrap();
    let projected = reader.host_lifecycle(&f.account.id, NOW).unwrap();
    assert_eq!(record, projected);
}

#[test]
fn tampered_or_denormalized_records_fail_digest_or_shape_checks() {
    let f = fixture();
    let run = f
        .store
        .prepare_run(&f.session.id, f.session.revision, 3)
        .unwrap();
    let started = f.store.mark_spawned(&run, 42).unwrap();
    orphaned(&f.store, &started);
    let record = f.store.host_lifecycle(&f.account.id, NOW).unwrap();

    // Any byte change breaks the digest binding.
    let mut tampered = record.clone();
    tampered["owner"] = json!("other");
    assert!(algal::host_contract::parse_host_lifecycle(&tampered).is_err());

    // An uncertain record cannot drop its pending intent.
    let mut dropped = record.clone();
    dropped["pendingIntent"] = Value::Null;
    assert!(algal::host_contract::parse_host_lifecycle(&dropped).is_err());

    // Uncertainty never projects as settled or failed — and the contract
    // rejects pendingIntent on terminal states outright.
    for state in ["settled", "failed"] {
        let mut collapsed = record.clone();
        collapsed["state"] = json!(state);
        assert!(algal::host_contract::parse_host_lifecycle(&collapsed).is_err());
    }

    // Set-valued lists must stay sorted and unique.
    let mut unsorted = record.clone();
    unsorted["heldAuthority"] = json!(["subscription-account", "account-lease"]);
    assert!(algal::host_contract::parse_host_lifecycle(&unsorted).is_err());
}

#[test]
fn committed_host_profile_matches_the_builder_and_passes_parse() {
    let built = context_recipe_host_profile().unwrap();
    algal::host_contract::parse_host_profile(&built).unwrap();
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    algal::host_contract::parse_host_profile(&fixture).unwrap();
    assert_eq!(fixture, built);
    assert_eq!(
        algal::canonical::canonical(&fixture).unwrap(),
        algal::canonical::canonical(&built).unwrap()
    );
    assert_eq!(built["uncertainEffectPolicy"], "reconcile-required");
    assert_eq!(built["usageUnits"]["name"], "subscription-quota");
}

#[test]
fn host_profile_evidence_and_evaluator_bind_committed_facts() {
    let built = context_recipe_host_profile().unwrap();
    let probes = built["probes"].as_array().unwrap();
    let replay: Value = serde_json::from_str(REPLAY).unwrap();
    let replay_digest = algal::canonical::digest(&replay).unwrap();
    let offline = probes
        .iter()
        .find(|probe| probe["id"] == "offline-replay")
        .unwrap();
    assert_eq!(offline["status"], "passed");
    assert_eq!(offline["evidence"], json!(replay_digest));
    for probe in probes {
        if probe["id"] != "offline-replay" {
            assert_eq!(probe["status"], "not-run");
            assert_eq!(probe["evidence"], Value::Null);
        }
    }

    // The evaluator digest names the exact pinned `algal` revision.
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(manifest.contains(ALGAL_EVALUATOR_REV));
    let expected = record_digest(&json!({
        "contract": "xcb.evaluator.v1",
        "evaluator": "algal",
        "rev": ALGAL_EVALUATOR_REV,
    }))
    .unwrap();
    assert_eq!(built["runtime"]["evaluatorDigest"], json!(expected));
}

#[test]
fn replay_evidence_digest_is_current() {
    let replay: Value = serde_json::from_str(REPLAY).unwrap();
    let digest = algal::canonical::digest(&replay).unwrap();
    assert_eq!(digest, REPLAY_EVIDENCE, "regenerate REPLAY_EVIDENCE");
}
