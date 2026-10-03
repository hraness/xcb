//! Supervisor reliability: a start that fails reaches the client that
//! spawned it, one damaged row never stops dispatch, shutdown is bounded,
//! repeating faults stay cheap, and a read-only store never migrates,
//! cleans or waits.

use super::*;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use xcb_core::models::{Mode, ModelChoice};

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    state: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = xcb_core::canonical(root.path()).unwrap();
    let state = private::directory(&base.join("state")).unwrap();
    Fixture {
        _root: root,
        base,
        state,
    }
}

fn model() -> ModelChoice {
    ModelChoice {
        provider: Provider::Claude,
        id: Id::new("fixture-model").unwrap(),
        label: "Fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: now_ms(),
    }
}

/// A stand-in for `xcb managed-daemon`, run as `<script> --state <root>
/// managed-daemon`; `$state` is the state root.
fn fake_daemon(f: &Fixture, body: &str) -> PathBuf {
    let path = f.base.join(format!("daemon-{}", new_id("fake").as_str()));
    fs::write(
        &path,
        format!("#!/bin/sh\nstate=\"$2\"\numask 077\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// Retry a spawn that raced another test thread's fork holding the freshly
/// written script open (ETXTBSY); nothing else is retried.
fn spawn_retrying(mut ensure: impl FnMut() -> Result<()>) -> Result<()> {
    for _ in 0..20 {
        match ensure() {
            Err(Error::LaunchNotStarted(error)) if error.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
    ensure()
}

#[tokio::test]
async fn a_supervisor_that_cannot_open_its_store_records_why_before_exiting() {
    let f = fixture();
    drop(ManagedStore::open(&f.state).unwrap());
    let raw = rusqlite::Connection::open(f.state.join("managed/managed.sqlite")).unwrap();
    raw.pragma_update(None, "user_version", 99).unwrap();
    drop(raw);
    let error = daemon(f.state.clone()).await.unwrap_err();
    assert!(error.to_string().contains("newer xcb"), "{error}");
    let fault = supervisor_fault(&f.state.join("managed")).unwrap();
    assert!(fault.starts_with(STARTUP_FAULT_PREFIX), "{fault}");
    assert!(fault.contains("newer xcb"), "{fault}");
    // The failed start never published an identity a client could trust.
    assert!(!f.state.join("managed/supervisor.identity.json").exists());
}

#[test]
fn ensure_daemon_returns_the_reason_a_spawned_supervisor_recorded() {
    let f = fixture();
    let failing = fake_daemon(
        &f,
        r#"printf '{"version":1,"at_ms":99999999999999,"message":"startup failed: unavailable: managed state was written by a newer xcb"}' > "$state/managed/supervisor.fault.json"
exit 1"#,
    );
    let error = spawn_retrying(|| ensure_daemon(&f.state, &failing))
        .unwrap_err()
        .to_string();
    assert!(error.contains("stopped while starting"), "{error}");
    assert!(
        error.contains("managed state was written by a newer xcb"),
        "{error}"
    );
    assert!(!error.contains(STARTUP_FAULT_PREFIX), "{error}");
    // No recorded reason: the exit status is named instead of a silent Ok.
    let silent = fake_daemon(&f, "exit 3");
    fs::remove_file(f.state.join("managed").join(SUPERVISOR_FAULT_FILE)).unwrap();
    let error = spawn_retrying(|| ensure_daemon(&f.state, &silent))
        .unwrap_err()
        .to_string();
    assert!(error.contains("status 3"), "{error}");
    // A clean exit means another supervisor won the lock.
    let lost_race = fake_daemon(&f, "exit 0");
    spawn_retrying(|| ensure_daemon(&f.state, &lost_race)).unwrap();
}

#[test]
fn ensure_daemon_confirms_a_registered_supervisor_and_bounds_a_slow_start() {
    let f = fixture();
    // Registers at once: confirmed well inside the window.
    let registering = fake_daemon(
        &f,
        &format!(
            r#"printf '{{"version":1,"pid":%d,"executable":"/bin/sh","sha256":"{}","package_version":"0.0.0"}}' "$$" > "$state/managed/supervisor.identity.json"
exec sleep 3"#,
            "a".repeat(64)
        ),
    );
    let started = Instant::now();
    spawn_retrying(|| ensure_daemon(&f.state, &registering)).unwrap();
    assert!(
        started.elapsed() < DAEMON_CONFIRM,
        "registration ends the wait"
    );
    // Still starting when the window closes: left running, never an error,
    // and the wait is bounded for the terminal that called it.
    fs::remove_file(f.state.join("managed/supervisor.identity.json")).unwrap();
    let slow = fake_daemon(&f, "exec sleep 2");
    let started = Instant::now();
    spawn_retrying(|| ensure_daemon_within(&f.state, &slow, Duration::from_millis(200))).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn ensure_daemon_honors_the_exact_installed_watchdog_declaration() {
    let f = fixture();
    let binary = fake_daemon(
        &f,
        r#"printf '%s' "$3" > "$state/observed-command"
printf '%s' "$XCB_SERVICE_INSTANCE" > "$state/observed-instance"
exit 0"#,
    );
    let home = private::directory(&f.base.join("home")).unwrap();
    let service = crate::habitat_service::Service::plan(&f.state, &binary, &home).unwrap();
    private::directory(service.manifest.parent().unwrap()).unwrap();
    private::create(
        &f.state.join("habitat-service.json"),
        &serde_json::to_vec(&service).unwrap(),
    )
    .unwrap();
    private::create(&service.manifest, service.render().unwrap().as_bytes()).unwrap();
    spawn_retrying(|| ensure_daemon(&f.state, &binary)).unwrap();
    assert_eq!(
        fs::read_to_string(f.state.join("observed-command")).unwrap(),
        "service-run"
    );
    assert!(
        uuid::Uuid::parse_str(&fs::read_to_string(f.state.join("observed-instance")).unwrap())
            .is_ok()
    );
    // Preserve an edited declaration and refuse to bypass it by launching an
    // unmonitored daemon through this client.
    fs::write(&service.manifest, b"foreign service declaration").unwrap();
    assert!(ensure_daemon(&f.state, &binary).is_err());
    assert_eq!(
        fs::read(&service.manifest).unwrap(),
        b"foreign service declaration"
    );
}

#[tokio::test]
async fn a_bad_schedule_row_or_daemon_store_never_stops_dispatch() {
    let f = fixture();
    let scheduled = private::directory(&f.base.join("scheduled")).unwrap();
    let other = private::directory(&f.base.join("other")).unwrap();
    let managed = Arc::new(ManagedStore::open(&f.state).unwrap());
    let store = Arc::new(Store::open(&f.state).unwrap());
    let thread = managed.global_thread().await.unwrap().id;
    managed
        .create_schedule_at(
            &thread,
            Some(&scheduled),
            "Check the scheduled project".into(),
            60_000,
            now_ms(),
        )
        .await
        .unwrap();
    let chat = managed.create_conversation(&other).await.unwrap().id;
    let task = managed
        .create_task(
            &chat,
            new_id("m"),
            "inspect the build".into(),
            vec![],
            &other,
        )
        .await
        .unwrap();
    // The due schedule's conversation no longer decodes.
    managed
        .db()
        .unwrap()
        .execute(
            "UPDATE conversations SET payload='{corrupt' WHERE id=?1",
            [thread.as_str()],
        )
        .unwrap();
    let mut supervisor = Supervisor::new(managed.clone(), store.clone());
    // More passes than the supervisor's own fault limit: none fails.
    for _ in 0..=MAX_TICK_FAULTS {
        supervisor.tick(false).await.unwrap();
    }
    let queued = managed.task(&task.id).unwrap().unwrap();
    assert!(
        queued.detail.starts_with(NO_ACCOUNT_DETAIL),
        "dispatch still ran: {}",
        queued.detail
    );
    let fault = supervisor_fault(managed.root()).unwrap();
    assert!(fault.contains("was skipped"), "{fault}");
    // A daemon store with an entry it cannot read pauses daemons only.
    let processes = managed.root().join("daemons/processes");
    fs::create_dir_all(&processes).unwrap();
    fs::write(processes.join("not-a-process"), b"").unwrap();
    for _ in 0..=MAX_TICK_FAULTS {
        supervisor.tick(false).await.unwrap();
    }
    let fault = supervisor_fault(managed.root()).unwrap();
    assert!(fault.contains("daemons are paused"), "{fault}");
    let other_work = managed
        .create_task(&chat, new_id("m"), "run the tests".into(), vec![], &other)
        .await
        .unwrap();
    supervisor.tick(false).await.unwrap();
    assert!(
        managed
            .task(&other_work.id)
            .unwrap()
            .unwrap()
            .detail
            .starts_with(NO_ACCOUNT_DETAIL)
    );
}

#[tokio::test]
async fn shutdown_stops_waiting_for_a_wedged_worker_and_keeps_its_records() {
    let f = fixture();
    let work = private::directory(&f.base.join("work")).unwrap();
    let managed = Arc::new(ManagedStore::open(&f.state).unwrap());
    let store = Arc::new(Store::open(&f.state).unwrap());
    let account = store
        .add_account(Provider::Claude, "Fixture", now_ms(), None)
        .unwrap();
    let session = store
        .create_session(&account.id, model(), &work, now_ms())
        .unwrap();
    let chat = managed.create_conversation(&work).await.unwrap();
    let task = managed
        .create_task(&chat.id, new_id("m"), "long work".into(), vec![], &work)
        .await
        .unwrap();
    let running = managed
        .prepare(
            &task,
            session.id.clone(),
            "claude/fixture-model".into(),
            "fixture route".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let mut supervisor = Supervisor::new(managed.clone(), store.clone());
    let (cancel, cancelled) = watch::channel(false);
    supervisor.active.insert(running.id.clone(), cancel);
    // A worker that never finishes, even once cancelled.
    supervisor.joins.spawn(async move {
        let _cancelled = cancelled;
        std::future::pending::<Completion>().await
    });
    let started = Instant::now();
    supervisor.shutdown_within(Duration::from_millis(200)).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let fault = supervisor_fault(managed.root()).unwrap();
    assert!(fault.contains("did not stop"), "{fault}");
    // Nothing was settled or released for the wedged worker: the next
    // supervisor start reconciles it from these records.
    let after = managed.task(&running.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Running);
    assert_eq!(after.revision, running.revision);
    assert!(store.session(&session.id).unwrap().is_some());
}

#[test]
fn a_repeating_fault_is_written_once_a_minute_even_between_others() {
    let f = fixture();
    let managed = private::directory(&f.state.join("managed")).unwrap();
    record_supervisor_fault(&managed, "first fault");
    let first = supervisor_fault_record(&managed).unwrap();
    record_supervisor_fault(&managed, "first fault");
    assert_eq!(supervisor_fault_record(&managed).unwrap(), first);
    record_supervisor_fault(&managed, "second fault");
    assert_eq!(supervisor_fault(&managed).as_deref(), Some("second fault"));
    // The first fault recurs between others: not rewritten within the minute.
    record_supervisor_fault(&managed, "first fault");
    assert_eq!(supervisor_fault(&managed).as_deref(), Some("second fault"));
    // A new fault is always written, and so is a cleared file.
    record_supervisor_fault(&managed, "third fault");
    assert_eq!(supervisor_fault(&managed).as_deref(), Some("third fault"));
    clear_supervisor_fault(&managed);
    record_supervisor_fault(&managed, "third fault");
    assert_eq!(supervisor_fault(&managed).as_deref(), Some("third fault"));
}

#[tokio::test]
async fn a_read_only_store_reads_managed_sessions_without_migrating_or_waiting() {
    let f = fixture();
    let work = private::directory(&f.base.join("work")).unwrap();
    let managed = ManagedStore::open(&f.state).unwrap();
    let store = Store::open(&f.state).unwrap();
    let account = store
        .add_account(Provider::Claude, "Fixture", now_ms(), None)
        .unwrap();
    let idle = store
        .create_session(&account.id, model(), &work, now_ms() - 10_000)
        .unwrap();
    let busy = store
        .create_session(&account.id, model(), &work, now_ms() - 10_000)
        .unwrap();
    let chat = managed.create_conversation(&work).await.unwrap();
    let task = managed
        .create_task(
            &chat.id,
            new_id("m"),
            "keep this session".into(),
            vec![],
            &work,
        )
        .await
        .unwrap();
    managed
        .prepare(
            &task,
            busy.id.clone(),
            "claude/fixture-model".into(),
            "fixture route".into(),
            0,
            String::new(),
        )
        .await
        .unwrap();
    let managed_root = managed.root().to_path_buf();
    drop(managed);
    // A running supervisor holds its lock for its whole life, and retention
    // is due, so a writable open would clean up right now.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(managed_root.join("supervisor.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let _ = fs::remove_file(managed_root.join(RETENTION_STAMP_FILE));
    let reader = Store::open_read_only(&f.state).unwrap();
    let started = Instant::now();
    let candidates = reader.prune_candidates(now_ms() + 60_000, 100).unwrap();
    assert!(started.elapsed() < Duration::from_millis(750));
    assert!(candidates.contains(&idle.id));
    assert!(!candidates.contains(&busy.id), "an active task's session");
    assert!(reader.remove_session(&busy.id).is_err());
    assert!(
        !managed_root.join(RETENTION_STAMP_FILE).exists(),
        "a read-only open never runs retention"
    );
    // An older schema is refused at once: no migration, no backup, and no
    // wait on the upgrade guard the running supervisor holds.
    let raw = rusqlite::Connection::open(managed_root.join("managed.sqlite")).unwrap();
    raw.pragma_update(None, "user_version", 6).unwrap();
    let reader = Store::open_read_only(&f.state).unwrap();
    let started = Instant::now();
    let error = reader
        .prune_candidates(now_ms() + 60_000, 100)
        .unwrap_err()
        .to_string();
    assert!(
        started.elapsed() < Duration::from_millis(750),
        "no guard wait"
    );
    assert!(error.contains("older xcb"), "{error}");
    let version: u32 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    assert!(!fs::read_dir(&managed_root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("managed.pre-v7")
    }));
    drop(lock);
}
