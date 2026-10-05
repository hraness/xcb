//! A small parent for one exact supervisor child. Only the Child handle is
//! signal authority; identities, lock holders and remembered PIDs are not.
//! The service manager owns restart backoff. Worker recovery remains the
//! managed runtime's responsibility, including after an uncertain crash.

use crate::{Error, Result, managed_supervisor, os, private, process};
use std::{
    fs::{File, OpenOptions},
    future::Future,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime},
};
use tokio::{io::AsyncReadExt, process::Child};

const LOG_BYTES: u64 = 2 * 1024 * 1024;
const LOG_WINDOW_BYTES: usize = 256 * 1024;
const LOG_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct Timing {
    poll: Duration,
    startup_grace: Duration,
    stale: Duration,
    confirmation: Duration,
    wake_grace: Duration,
    shutdown: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(5),
            // Startup reconciliation is bounded but can legitimately take
            // several minutes when a durable state root contains a large
            // history or retained recovery records.  Keep the child under
            // custody while it publishes its first heartbeat; restarting it
            // at the old three-minute mark could livelock startup forever.
            startup_grace: Duration::from_secs(600),
            stale: Duration::from_secs(120),
            confirmation: Duration::from_secs(30),
            wake_grace: Duration::from_secs(30),
            shutdown: Duration::from_secs(45),
        }
    }
}

#[derive(Default)]
struct HeartbeatProgress(Option<(SystemTime, Instant)>);

impl HeartbeatProgress {
    fn observe(&mut self, marker: SystemTime, now: Instant) -> Duration {
        match self.0 {
            Some((previous, since)) if previous == marker => now.saturating_duration_since(since),
            _ => {
                self.0 = Some((marker, now));
                Duration::ZERO
            }
        }
    }

    fn reset(&mut self) {
        self.0 = None;
    }
}

pub fn log_path(root: &Path) -> PathBuf {
    root.join("managed/service-logs/supervisor.log")
}

/// Bound both disk space (three files) and steady-state writes. Open handles
/// stay pinned and are checked against their names before any write. Rotation
/// copies between those handles; it never unlinks a path or follows a link.
struct Logs {
    paths: [PathBuf; 3],
    files: [File; 3],
    limit: u64,
    window: Instant,
    written: usize,
}

impl Logs {
    fn open(root: &Path, limit: u64) -> Result<Self> {
        let directory = private::directory(&root.join("managed/service-logs"))?;
        let paths = [
            directory.join("supervisor.log"),
            directory.join("supervisor.1.log"),
            directory.join("supervisor.2.log"),
        ];
        let open = |path: &Path| -> Result<File> {
            let file = os::no_follow(
                os::owner_only(
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false),
                ),
                true,
            )
            .open(path)?;
            private::check_file(&file, limit)?;
            private::same_file(path, &file)?;
            Ok(file)
        };
        let files = [open(&paths[0])?, open(&paths[1])?, open(&paths[2])?];
        Ok(Self {
            paths,
            files,
            limit,
            window: Instant::now(),
            written: 0,
        })
    }

    fn check(&self) -> Result<()> {
        private::check_directory(self.paths[0].parent().ok_or(Error::PrivateState)?)?;
        for (path, file) in self.paths.iter().zip(&self.files) {
            private::check_file(file, self.limit)?;
            private::same_file(path, file)?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> Result<()> {
        self.check()?;
        for source in (0..2).rev() {
            let (sources, destinations) = self.files.split_at_mut(source + 1);
            let from = &mut sources[source];
            let to = &mut destinations[0];
            from.seek(SeekFrom::Start(0))?;
            to.set_len(0)?;
            to.seek(SeekFrom::Start(0))?;
            std::io::copy(&mut from.take(self.limit), to)?;
        }
        self.files[0].set_len(0)?;
        self.files[0].seek(SeekFrom::Start(0))?;
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.window.elapsed() >= LOG_WINDOW {
            self.window = Instant::now();
            self.written = 0;
        }
        // Keep draining after the allowance is consumed so output cannot
        // stall a supervisor or grow an in-memory queue.
        let allowed = LOG_WINDOW_BYTES
            .saturating_sub(self.written)
            .min(bytes.len());
        self.written += allowed;
        self.write_bounded(&bytes[..allowed])
    }

    fn write_bounded(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            self.check()?;
            let length = self.files[0].metadata()?.len();
            if length >= self.limit {
                self.rotate()?;
                continue;
            }
            let count = bytes.len().min((self.limit - length) as usize);
            self.files[0].seek(SeekFrom::End(0))?;
            self.files[0].write_all(&bytes[..count])?;
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

/// A sink failure disables writes while the parent continues draining pipes
/// and watching the child. Disk-full must not cause an unbounded error log.
struct Output(Option<Logs>);

impl Output {
    fn write(&mut self, bytes: &[u8]) {
        if self
            .0
            .as_mut()
            .is_some_and(|logs| logs.write(bytes).is_err())
        {
            self.0 = None;
        }
    }

    fn note(&mut self, message: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.write(format!("\n[xcb service {now}] {message}\n").as_bytes());
    }
}

struct Ownership {
    root: PathBuf,
    instance: String,
    sha256: String,
    lock_path: PathBuf,
    lock: private::ExclusiveLock,
}

impl Ownership {
    fn check(&self) -> Result<()> {
        private::check_directory(self.lock_path.parent().ok_or(Error::PrivateState)?)?;
        private::check_file(self.lock.file(), 0)?;
        private::same_file(&self.lock_path, self.lock.file())
    }

    fn progress(&self, child: &Child) -> Option<Option<SystemTime>> {
        self.check().ok()?;
        managed_supervisor::service_child_progress(
            &self.root,
            child.id()?,
            &self.instance,
            &self.sha256,
        )
        .ok()
        .flatten()
    }
}

fn claim(root: &Path, sha256: String) -> Result<Ownership> {
    let directory = private::directory(&root.join("managed"))?;
    let lock_path = directory.join("service-watchdog.lock");
    let file = os::no_follow(
        os::owner_only(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        ),
        true,
    )
    .open(&lock_path)?;
    private::check_file(&file, 0)?;
    match file.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::Conflict(
                "an xcb service watchdog is already running",
            ));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let ownership = Ownership {
        root: root.to_owned(),
        instance: uuid::Uuid::new_v4().to_string(),
        sha256,
        lock_path,
        lock: private::ExclusiveLock::held(file),
    };
    ownership.check()?;
    Ok(ownership)
}

/// The service manager calls this command. It never adopts a daemon started
/// by another client or an earlier watchdog. The manager restarts this parent
/// after exit using the installed one-minute backoff.
pub async fn run(root: PathBuf) -> Result<i32> {
    let root = private::check_directory(&root)?;
    let (executable, sha256) = process::host_identity()?;
    let mut ownership = claim(&root, sha256)?;
    if let Ok(instance) = std::env::var("XCB_SERVICE_INSTANCE") {
        uuid::Uuid::parse_str(&instance)
            .map_err(|_| Error::Unavailable("invalid service launch identity"))?;
        ownership.instance = instance;
    }
    let output = Output(Some(Logs::open(&root, LOG_BYTES)?));
    let mut interrupt = os::Terminate::install()?;
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--state")
        .arg(&root)
        .arg("managed-daemon")
        .env("XCB_SERVICE_INSTANCE", &ownership.instance)
        .env("XCB_SERVICE_PARENT", std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // An unexpectedly lost parent does not signal provider descendants.
        .kill_on_drop(false);
    // launchd may clean the parent's process group when it exits. Give the
    // child its own group; this parent still signals only the child itself.
    os::detach(command.as_std_mut());
    let child = command.spawn().map_err(Error::LaunchNotStarted)?;
    supervise(child, ownership, output, Timing::default(), async {
        let _ = interrupt.recv().await;
    })
    .await
}

fn resumed(previous: SystemTime, now: SystemTime, elapsed: Duration, poll: Duration) -> bool {
    elapsed > poll.saturating_mul(3)
        || match now.duration_since(previous) {
            Ok(elapsed) => elapsed > poll.saturating_mul(3),
            Err(_) => true,
        }
}

/// Signal only a still-unreaped child. A held Child protects against PID reuse;
/// the identity file is deliberately not consulted for termination delivery.
async fn stop_child(child: &mut Child, graceful: bool, timeout: Duration) -> Result<i32> {
    if let Some(status) = child.try_wait()? {
        return Ok(status.code().unwrap_or(1));
    }
    if graceful {
        #[cfg(unix)]
        if let Some(pid) = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        if let Ok(status) = tokio::time::timeout(timeout, child.wait()).await {
            return Ok(status?.code().unwrap_or(1));
        }
    }
    // No process-group or descendant signal: uncertain worker custody stays
    // intact for startup reconciliation and explicit recovery.
    child.start_kill()?;
    let status = tokio::time::timeout(timeout.min(Duration::from_secs(10)), child.wait())
        .await
        .map_err(|_| {
            Error::Unavailable(
                "supervisor child exit could not be confirmed; worker recovery requirements remain",
            )
        })??;
    Ok(status.code().unwrap_or(1))
}

async fn supervise(
    mut child: Child,
    ownership: Ownership,
    mut output: Output,
    timing: Timing,
    stop: impl Future<Output = ()>,
) -> Result<i32> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or(Error::Unavailable("service stdout unavailable"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(Error::Unavailable("service stderr unavailable"))?;
    let mut out_open = true;
    let mut err_open = true;
    let mut out = [0; 8192];
    let mut err = [0; 8192];
    let mut ticks = tokio::time::interval(timing.poll);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut grace_until = Instant::now() + timing.startup_grace;
    let mut last_poll = SystemTime::now();
    let mut last_poll_instant = Instant::now();
    let mut progress = HeartbeatProgress::default();
    let mut stale_since = None;
    let mut lost_noted = false;
    tokio::pin!(stop);
    output.note("watching the supervisor child; output is limited to 256 KiB per minute and three 2 MiB files");
    let code = loop {
        tokio::select! {
            read = stdout.read(&mut out), if out_open => match read {
                Ok(0) | Err(_) => out_open = false,
                Ok(count) => output.write(&out[..count]),
            },
            read = stderr.read(&mut err), if err_open => match read {
                Ok(0) | Err(_) => err_open = false,
                Ok(count) => output.write(&err[..count]),
            },
            _ = &mut stop => {
                output.note("service stop requested; stopping only its own supervisor child");
                break stop_child(&mut child, true, timing.shutdown).await?;
            },
            _ = ticks.tick() => {
                if let Some(status) = child.try_wait()? {
                    break status.code().unwrap_or(1);
                }
                let wall_now = SystemTime::now();
                let now = Instant::now();
                if resumed(last_poll, wall_now, now.saturating_duration_since(last_poll_instant), timing.poll) {
                    grace_until = now + timing.wake_grace;
                    stale_since = None;
                    progress.reset();
                }
                last_poll = wall_now;
                last_poll_instant = now;
                match ownership.progress(&child) {
                    Some(Some(marker)) => {
                        // Only an unchanged verified marker over monotonic time
                        // can trigger recovery. Wall-clock steps cannot hide a
                        // hung child or make a progressing one look stalled.
                        let unchanged = progress.observe(marker, now);
                        lost_noted = false;
                        if now < grace_until || unchanged <= timing.stale {
                            stale_since = None;
                            continue;
                        }
                        let since = stale_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= timing.confirmation {
                            // Revalidate the enrollment immediately before the
                            // effect. Loss of identity or lock fails closed.
                            if ownership.progress(&child) == Some(Some(marker)) {
                                output.note("supervisor heartbeat stopped; restarting only the owned child; worker recovery requirements remain");
                                let _ = stop_child(&mut child, false, timing.shutdown).await?;
                                break 1;
                            }
                            stale_since = None;
                        }
                    }
                    _ => {
                        stale_since = None;
                        progress.reset();
                        if now >= grace_until && !lost_noted {
                            output.note("supervisor progress cannot be bound to this child; automatic restart is suspended");
                            lost_noted = true;
                        }
                    }
                }
            },
        }
    };
    // Capture final output, but a descendant retaining a pipe cannot keep
    // this service alive. No output is accumulated in memory.
    let _ = tokio::time::timeout(Duration::from_millis(250), async {
        while out_open || err_open {
            tokio::select! {
                read = stdout.read(&mut out), if out_open => match read {
                    Ok(0) | Err(_) => out_open = false,
                    Ok(count) => output.write(&out[..count]),
                },
                read = stderr.read(&mut err), if err_open => match read {
                    Ok(0) | Err(_) => err_open = false,
                    Ok(count) => output.write(&err[..count]),
                },
            }
        }
    })
    .await;
    output.note("supervisor child exited; service manager controls the next start");
    Ok(code)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, PathBuf, Ownership) {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let ownership = claim(&root, "a".repeat(64)).unwrap();
        (directory, root, ownership)
    }

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(5),
            startup_grace: Duration::ZERO,
            stale: Duration::from_millis(20),
            confirmation: Duration::from_millis(20),
            wake_grace: Duration::from_millis(20),
            shutdown: Duration::from_millis(50),
        }
    }

    fn sleeping_child() -> Child {
        tokio::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    fn enroll(ownership: &Ownership, pid: u32, instance: &str) {
        let record = serde_json::json!({
            "version": 1,
            "pid": pid,
            "executable": "/bin/sleep",
            "sha256": ownership.sha256,
            "package_version": env!("CARGO_PKG_VERSION"),
            "service_instance": instance,
        });
        private::create(
            &ownership.root.join("managed/supervisor.identity.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let heartbeat = ownership.root.join("managed/supervisor.heartbeat");
        private::create(&heartbeat, &[]).unwrap();
        File::options()
            .write(true)
            .open(heartbeat)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(60))
            .unwrap();
    }

    #[tokio::test]
    async fn a_hung_enrolled_child_exits_without_touching_worker_evidence() {
        let (_directory, root, ownership) = fixture();
        let child = sleeping_child();
        enroll(&ownership, child.id().unwrap(), &ownership.instance);
        let evidence = root.join("managed/task-evidence");
        private::create(&evidence, b"uncertain worker custody").unwrap();
        let code = tokio::time::timeout(
            Duration::from_secs(2),
            supervise(
                child,
                ownership,
                Output(None),
                fast(),
                std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(
            private::read(&evidence, 128).unwrap(),
            b"uncertain worker custody"
        );
    }

    #[tokio::test]
    async fn an_unchanging_future_heartbeat_cannot_hide_a_hung_owned_child() {
        let (_directory, root, ownership) = fixture();
        let child = sleeping_child();
        enroll(&ownership, child.id().unwrap(), &ownership.instance);
        File::options()
            .write(true)
            .open(root.join("managed/supervisor.heartbeat"))
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        let code = tokio::time::timeout(
            Duration::from_secs(2),
            supervise(
                child,
                ownership,
                Output(None),
                fast(),
                std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(code, 1);
    }

    #[test]
    fn marker_changes_and_interrupted_observation_reset_monotonic_progress() {
        let now = Instant::now();
        let future = SystemTime::now() + Duration::from_secs(3600);
        let mut progress = HeartbeatProgress::default();
        assert_eq!(progress.observe(future, now), Duration::ZERO);
        assert_eq!(
            progress.observe(future, now + Duration::from_secs(150)),
            Duration::from_secs(150)
        );
        // A backwards-moving marker is still an observed refresh.
        assert_eq!(
            progress.observe(
                future - Duration::from_secs(3600),
                now + Duration::from_secs(151)
            ),
            Duration::ZERO
        );
        progress.reset();
        assert_eq!(
            progress.observe(
                future - Duration::from_secs(3600),
                now + Duration::from_secs(900)
            ),
            Duration::ZERO
        );
    }

    #[tokio::test]
    async fn foreign_registration_never_signals_the_foreign_child() {
        let (_directory, _root, ownership) = fixture();
        let child = sleeping_child();
        let mut foreign = sleeping_child();
        enroll(&ownership, foreign.id().unwrap(), &ownership.instance);
        let started = Instant::now();
        supervise(
            child,
            ownership,
            Output(None),
            fast(),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(90));
        assert!(foreign.try_wait().unwrap().is_none());
        foreign.kill().await.unwrap();
    }

    #[tokio::test]
    async fn lost_watchdog_lock_suspends_automatic_termination() {
        let (_directory, root, ownership) = fixture();
        let child = sleeping_child();
        enroll(&ownership, child.id().unwrap(), &ownership.instance);
        fs::rename(&ownership.lock_path, root.join("managed/preserved-lock")).unwrap();
        private::create(&ownership.lock_path, &[]).unwrap();
        let started = Instant::now();
        supervise(
            child,
            ownership,
            Output(None),
            fast(),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(90));
    }

    #[tokio::test]
    async fn own_pid_without_the_launch_nonce_is_not_an_enrolled_child() {
        let (_directory, _root, ownership) = fixture();
        let child = sleeping_child();
        enroll(
            &ownership,
            child.id().unwrap(),
            &uuid::Uuid::new_v4().to_string(),
        );
        let started = Instant::now();
        supervise(
            child,
            ownership,
            Output(None),
            fast(),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(90));
    }

    #[tokio::test]
    async fn an_exited_child_is_reaped_and_final_output_is_bounded() {
        let (_directory, root, ownership) = fixture();
        let child = tokio::process::Command::new("/usr/bin/printf")
            .arg("%s")
            .arg("x".repeat(64 * 1024))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let output = Output(Some(Logs::open(&root, 1024).unwrap()));
        let result = supervise(child, ownership, output, fast(), std::future::pending())
            .await
            .unwrap();
        assert_eq!(result, 0);
        let logs = Logs::open(&root, 1024).unwrap();
        assert!(
            logs.files
                .iter()
                .all(|file| file.metadata().unwrap().len() <= 1024)
        );
        assert_eq!(logs.files[1].metadata().unwrap().len(), 1024);
    }

    #[test]
    fn log_rotation_retains_only_the_last_three_files_and_rejects_links() {
        let (_directory, root, _ownership) = fixture();
        let mut logs = Logs::open(&root, 128).unwrap();
        for number in 0..20 {
            logs.write_bounded(&[number; 128]).unwrap();
        }
        for (index, path) in logs.paths.iter().enumerate() {
            assert_eq!(fs::read(path).unwrap(), vec![19 - index as u8; 128]);
        }
        let path = logs.paths[0].clone();
        fs::rename(&path, path.with_extension("saved")).unwrap();
        let foreign = root.join("foreign-data");
        fs::write(&foreign, b"preserve me").unwrap();
        std::os::unix::fs::symlink(&foreign, &path).unwrap();
        assert!(logs.write_bounded(b"do not write").is_err());
        assert!(Logs::open(&root, 128).is_err());
        assert_eq!(fs::read(foreign).unwrap(), b"preserve me");
    }

    #[test]
    fn output_floods_have_a_fixed_write_allowance() {
        let (_directory, root, _ownership) = fixture();
        let mut logs = Logs::open(&root, LOG_BYTES).unwrap();
        let bytes = vec![b'x'; 8192];
        for _ in 0..100 {
            logs.write(&bytes).unwrap();
        }
        assert_eq!(
            logs.files[0].metadata().unwrap().len(),
            LOG_WINDOW_BYTES as u64
        );
        logs.window = Instant::now() - LOG_WINDOW;
        logs.write(b"resumed").unwrap();
        assert_eq!(
            logs.files[0].metadata().unwrap().len(),
            LOG_WINDOW_BYTES as u64 + 7
        );
    }

    #[test]
    fn duplicate_watchdogs_are_refused_and_wake_resets_confirmation() {
        let (_directory, root, _ownership) = fixture();
        assert!(matches!(
            claim(&root, "a".repeat(64)),
            Err(Error::Conflict(_))
        ));
        let now = SystemTime::now();
        let poll = Duration::from_secs(5);
        assert!(!resumed(now, now + poll, poll, poll));
        assert!(resumed(now, now + Duration::from_secs(600), poll, poll));
        assert!(resumed(now, now - Duration::from_secs(1), poll, poll));
        // A clock correction can conceal the wall-clock gap while a paused
        // scheduler still needs the same wake grace and fresh observation.
        assert!(resumed(now, now + poll, Duration::from_secs(600), poll));
    }
}
