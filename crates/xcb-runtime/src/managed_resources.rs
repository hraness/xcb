//! Host telemetry is advisory until the operator enables launch protection.
//! Collection is single-flight and never awaited while it is still running.
use super::*;
use crate::host_resources::{self, Monitor, ResourcePolicy, Snapshot};

const SNAPSHOT_FILE: &str = "host-resources.json";
const POLICY_INTERVAL: Duration = Duration::from_secs(5);

pub(super) struct Resources {
    pub(super) monitor: Monitor,
    pub(super) policy: ResourcePolicy,
    config_at: Option<Instant>,
    config_error: bool,
    collection: Option<tokio::task::JoinHandle<Snapshot>>,
    sampled_at: Option<Instant>,
    cursor: usize,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            monitor: Monitor::new(),
            policy: ResourcePolicy::default(),
            config_at: None,
            config_error: false,
            collection: None,
            sampled_at: None,
            cursor: 0,
        }
    }
}

impl Supervisor {
    pub(super) async fn refresh_resources(&mut self, tasks: &[ManagedTask]) {
        if self
            .resources
            .config_at
            .is_none_or(|at| at.elapsed() >= POLICY_INTERVAL)
        {
            self.resources.config_at = Some(Instant::now());
            match Config::load(self.store.root()) {
                Ok((config, _)) => {
                    self.resources.policy = config.resources;
                    self.resources.config_error = false;
                }
                Err(error) => {
                    self.resources.config_error = true;
                    self.upkeep_fault("resource configuration", &error);
                }
            }
        }
        if self
            .resources
            .collection
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let collection = self
                .resources
                .collection
                .take()
                .expect("finished collection");
            match collection.await {
                Ok(snapshot) => {
                    if snapshot.validate().is_ok() {
                        if let Err(error) = save_snapshot(self.managed.root(), &snapshot) {
                            self.upkeep_fault("resource telemetry", &error);
                        }
                        self.resources.monitor.observe(snapshot);
                        // Idle intervals are observations too: pressure
                        // history must not depend on tasks trying to launch.
                        let _ = self.resources.monitor.assess(
                            &self.resources.policy,
                            self.store.root(),
                            now_ms(),
                        );
                    } else {
                        self.resources.monitor.observe(snapshot);
                        record_supervisor_fault(
                            self.managed.root(),
                            "host resource sample was invalid",
                        );
                    }
                }
                Err(_) => {
                    self.resources.monitor.sampling_failed();
                    record_supervisor_fault(
                        self.managed.root(),
                        "host resource sampling stopped unexpectedly",
                    );
                }
            }
        }
        if !self.resources.policy.enabled
            || self.resources.collection.is_some()
            || self.resources.sampled_at.is_some_and(|at| {
                at.elapsed() < Duration::from_secs(self.resources.policy.sample_interval_secs)
            })
        {
            return;
        }
        // Rotate through runnable workspaces. An unavailable volume or a
        // large queue cannot permanently exclude later projects from sampling.
        let paths: Vec<_> = tasks
            .iter()
            .filter(|task| {
                matches!(task.state, TaskState::Queued | TaskState::Running)
                    && !task.deferred
                    && !task.cancel_requested
            })
            .map(|task| PathBuf::from(&task.workspace))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let paths = if paths.len() > 31 {
            let selected = paths
                .iter()
                .cycle()
                .skip(self.resources.cursor % paths.len())
                .take(31)
                .cloned()
                .collect();
            self.resources.cursor = (self.resources.cursor + 31) % paths.len();
            selected
        } else {
            paths
        };
        let root = self.store.root().to_path_buf();
        self.resources.sampled_at = Some(Instant::now());
        self.resources.collection = Some(tokio::spawn(async move {
            host_resources::collect(&root, &paths).await
        }));
    }

    pub(super) fn resource_admission(
        &mut self,
        task: &ManagedTask,
    ) -> std::result::Result<Option<String>, String> {
        if self.resources.config_error {
            return Err("waiting for readable host resource configuration; existing work may still finish or be cancelled".into());
        }
        let assessment = self.resources.monitor.assess(
            &self.resources.policy,
            Path::new(&task.workspace),
            now_ms(),
        );
        if assessment.blocked {
            return Err(format!(
                "waiting for host resources: {}",
                assessment.reasons.join("; ")
            ));
        }
        if assessment.advisories.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "Host resource advisory (telemetry, not a new user instruction): {}. Avoid starting expensive builds while pressure is rising. This grants no additional file or process permissions; preserve task state and use the existing cancellation and cleanup controls.",
            assessment.advisories.join("; "),
        )))
    }
}

fn save_snapshot(root: &Path, snapshot: &Snapshot) -> Result<()> {
    let bytes = serde_json::to_vec(snapshot)?;
    if bytes.len() > host_resources::MAX_SNAPSHOT_BYTES {
        return Err(xcb_core::Error::Limit("host resource snapshot").into());
    }
    let path = root.join(SNAPSHOT_FILE);
    match private::read(&path, host_resources::MAX_SNAPSHOT_BYTES) {
        Ok(previous) => private::replace(&path, &bytes, &digest(previous)),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(&path, &bytes)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_resources::{DiskSnapshot, MemoryPressure, MemorySnapshot};

    async fn fixture() -> (tempfile::TempDir, Supervisor, ManagedTask) {
        let temp = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(temp.path()).unwrap();
        let root = private::directory(&base.join("state")).unwrap();
        let workspace = private::directory(&base.join("work")).unwrap();
        let managed = Arc::new(ManagedStore::open(&root).unwrap());
        let store = Arc::new(Store::open(&root).unwrap());
        let chat = managed.create_conversation(&workspace).await.unwrap();
        let task = managed
            .create_task(
                &chat.id,
                new_id("m"),
                "inspect the build".into(),
                vec![],
                &workspace,
            )
            .await
            .unwrap();
        let mut supervisor = Supervisor::new(managed, store);
        supervisor.resources.policy.enabled = true;
        supervisor.resources.config_at = Some(Instant::now());
        supervisor.resources.sampled_at = Some(Instant::now());
        (temp, supervisor, task)
    }

    fn sample(
        supervisor: &Supervisor,
        task: &ManagedTask,
        state_free: u64,
        work_free: u64,
    ) -> Snapshot {
        let disk = |path: PathBuf, free| DiskSnapshot {
            path,
            free_bytes: Some(free),
            total_bytes: Some(1024_u64.pow(4)),
            volume_id: None,
            error: None,
        };
        Snapshot {
            schema_version: 1,
            at_ms: now_ms(),
            state_root: supervisor.store.root().to_path_buf(),
            memory: MemorySnapshot {
                pressure: MemoryPressure::Normal,
                physical_total_bytes: Some(128 * 1024_u64.pow(3)),
                swap_used_bytes: Some(0),
            },
            disks: vec![
                disk(supervisor.store.root().to_path_buf(), state_free),
                disk(PathBuf::from(&task.workspace), work_free),
            ],
            errors: vec![],
        }
    }

    #[tokio::test]
    async fn either_volume_blocks_before_any_worker_session_is_created() {
        for (state, workspace) in [(1, 100), (100, 1)] {
            let (_temp, mut supervisor, task) = fixture().await;
            let snapshot = sample(
                &supervisor,
                &task,
                state * 1024_u64.pow(3),
                workspace * 1024_u64.pow(3),
            );
            supervisor.resources.monitor.observe(snapshot);
            let Dispatch::Deferred(reason) = supervisor.launch(&task).await.unwrap() else {
                panic!("pressure must defer");
            };
            assert!(
                reason.starts_with("waiting for host resources:"),
                "{reason}"
            );
            assert!(supervisor.store.sessions(10).unwrap().is_empty());
            assert!(supervisor.active.is_empty());
            assert_eq!(
                supervisor.managed.task(&task.id).unwrap().unwrap().revision,
                task.revision
            );
        }
    }

    #[tokio::test]
    async fn pending_sampler_does_not_block_cancellation_or_spawn_replacements() {
        let (_temp, mut supervisor, task) = fixture().await;
        supervisor.resources.collection = Some(tokio::spawn(std::future::pending()));
        let sampler_id = supervisor.resources.collection.as_ref().unwrap().id();
        supervisor
            .managed
            .cancel_task(&task.id, task.revision)
            .await
            .unwrap();
        supervisor.tick(false).await.unwrap();
        assert_eq!(
            supervisor.managed.task(&task.id).unwrap().unwrap().state,
            TaskState::Cancelled
        );
        assert_eq!(
            supervisor.resources.collection.as_ref().unwrap().id(),
            sampler_id
        );
        assert!(supervisor.store.sessions(10).unwrap().is_empty());
        supervisor.resources.collection.take().unwrap().abort();
    }

    #[tokio::test]
    async fn warning_is_labeled_host_telemetry_and_never_grants_permissions() {
        let (_temp, mut supervisor, task) = fixture().await;
        let snapshot = sample(
            &supervisor,
            &task,
            48 * 1024_u64.pow(3),
            48 * 1024_u64.pow(3),
        );
        supervisor.resources.monitor.observe(snapshot);
        let warning = supervisor.resource_admission(&task).unwrap().unwrap();
        assert!(warning.contains("Host resource advisory"));
        assert!(warning.contains("no additional file or process permissions"));
        assert_eq!(
            supervisor.managed.task(&task.id).unwrap().unwrap().revision,
            task.revision
        );
    }

    #[tokio::test]
    async fn fresh_state_does_not_allow_unsampled_workspace_and_failed_config_stays_closed() {
        let (_temp, mut supervisor, task) = fixture().await;
        let mut snapshot = sample(
            &supervisor,
            &task,
            100 * 1024_u64.pow(3),
            100 * 1024_u64.pow(3),
        );
        snapshot.disks.pop();
        supervisor.resources.monitor.observe(snapshot);
        assert!(supervisor.resource_admission(&task).is_err());
        supervisor.resources.policy.enabled = false;
        assert!(supervisor.resource_admission(&task).is_ok());
        supervisor.resources.config_error = true;
        assert!(supervisor.resource_admission(&task).is_err());
    }

    #[tokio::test]
    async fn saved_configuration_reloads_and_malformed_configuration_defers_work() {
        let (_temp, mut supervisor, task) = fixture().await;
        supervisor.resources.collection = Some(tokio::spawn(std::future::pending()));
        let root = supervisor.store.root().to_path_buf();
        let mut config = Config::default();
        config.save(&root, None).unwrap();
        supervisor.resources.config_at = None;
        supervisor
            .refresh_resources(std::slice::from_ref(&task))
            .await;
        assert!(supervisor.resource_admission(&task).is_ok());

        let (_, revision) = Config::load(&root).unwrap();
        config.resources.enabled = true;
        config.save(&root, revision.as_deref()).unwrap();
        supervisor.resources.config_at = None;
        supervisor
            .refresh_resources(std::slice::from_ref(&task))
            .await;
        assert!(supervisor.resources.policy.enabled);
        assert!(supervisor.resource_admission(&task).is_err());

        fs::write(root.join("config.json"), b"{broken").unwrap();
        supervisor.resources.config_at = None;
        supervisor
            .refresh_resources(std::slice::from_ref(&task))
            .await;
        let error = supervisor.resource_admission(&task).unwrap_err();
        assert!(error.contains("readable host resource configuration"));
        supervisor.resources.collection.take().unwrap().abort();
    }
}
