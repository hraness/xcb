//! A fixed read-only GitHub observer. It never merges, writes provider state,
//! or accepts arbitrary argv, URLs, credentials, or API paths from a worker.
use super::*;
use tokio::{process::Command, sync::watch};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrWatch {
    pub repository: String,
    pub number: u64,
    pub head: String,
    pub waiting: bool,
    pub polls: u32,
    pub observed_at_ms: Option<u64>,
    pub status: String,
}
impl PrWatch {
    pub(super) fn validate(&self) -> Result<()> {
        if !repository(&self.repository)
            || self.number == 0
            || self.number > 1_000_000_000
            || self.head.len() != 40
            || !self.head.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Conflict("invalid completion PR watch"));
        }
        bounded_text(&self.status, 256)?;
        Ok(())
    }
}
fn repository(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part.len() <= 100
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}
pub(super) fn request(report: &str, workspace_path: &str) -> Option<PrWatch> {
    let origin = workspace::github_origin(Path::new(workspace_path))?;
    for line in report.lines() {
        let Some(line) = line.trim().strip_prefix("WAIT_PR ") else {
            continue;
        };
        let fields: Vec<_> = line.split_whitespace().collect();
        let url = fields.iter().find_map(|field| field.strip_prefix("pr="))?;
        let tail = url.strip_prefix("https://github.com/")?;
        let (repo, number) = tail.split_once("/pull/")?;
        let head = fields
            .iter()
            .find_map(|field| field.strip_prefix("head="))?;
        if repo != origin {
            return None;
        }
        let target = PrWatch {
            repository: repo.into(),
            number: number.parse().ok()?,
            head: head.into(),
            waiting: true,
            polls: 0,
            observed_at_ms: None,
            status: "awaiting host observation".into(),
        };
        target.validate().ok()?;
        return Some(target);
    }
    None
}

fn response(
    target: &PrWatch,
    pull: &Value,
    checks: &Value,
    status: &Value,
) -> Result<(bool, &'static str)> {
    if pull["number"].as_u64() != Some(target.number)
        || pull["baseRepo"].as_str() != Some(target.repository.as_str())
    {
        return Err(Error::Protocol("PR observation identity changed"));
    }
    if pull["head"].as_str() != Some(target.head.as_str()) {
        return Ok((false, "PR head changed; worker must revalidate"));
    }
    if pull["merged"] == true {
        return Ok((false, "PR merged; worker must verify remaining delivery"));
    }
    if pull["state"] != "open" {
        return Ok((false, "PR closed; worker must reconcile requested delivery"));
    }
    if checks["total"].as_u64().is_none_or(|total| total > 100)
        || status["total"].as_u64().is_none_or(|total| total > 100)
    {
        return Ok((false, "check observation incomplete; worker must inspect"));
    }
    let runs = checks["runs"]
        .as_array()
        .ok_or(Error::Protocol("PR check observation schema"))?;
    if runs.iter().any(|run| {
        run["status"] == "completed"
            && !matches!(
                run["conclusion"].as_str(),
                Some("success" | "neutral" | "skipped")
            )
    }) || matches!(status["state"].as_str(), Some("failure" | "error"))
    {
        return Ok((false, "PR checks failed; worker must repair"));
    }
    if runs.iter().any(|run| run["status"] != "completed")
        || (status["state"] == "pending" && status["total"].as_u64().unwrap_or(0) > 0)
    {
        return Ok((true, "exact-head PR checks remain pending"));
    }
    Ok((
        false,
        "PR checks settled; worker must apply repository delivery gates",
    ))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserverCustody {
    version: u32,
    task: Id,
    owner: u32,
    instance: String,
    host: String,
    boot: String,
    pid: Option<u32>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn boot_identity() -> Result<String> {
    #[cfg(target_os = "macos")]
    let boot = {
        let mut command = Command::new("/usr/sbin/sysctl");
        command.args(["-n", "kern.bootsessionuuid"]).env_clear();
        let (_owner, cancel) = watch::channel(false);
        match crate::process::capture_supervised(
            command,
            256,
            Duration::from_secs(2),
            cancel,
            |_| Ok(()),
        )
        .await
        {
            crate::process::CaptureOutcome::Joined(Ok(bytes)) => {
                String::from_utf8(bytes.to_vec()).map_err(|_| Error::PrivateState)?
            }
            crate::process::CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
            _ => return Err(Error::Unavailable("host boot identity unavailable")),
        }
    };
    #[cfg(target_os = "linux")]
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let parsed = uuid::Uuid::parse_str(boot.trim()).map_err(|_| Error::PrivateState)?;
    Ok(parsed.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn boot_identity() -> Result<String> {
    Err(Error::Unavailable(
        "completion observer boot identity unsupported",
    ))
}

fn worker_budget_available(task: &ManagedTask, config: &Config) -> bool {
    config.extensions.auto_continue.enabled
        && task.attempts < task.max_attempts
        && task.attempts < config.extensions.auto_continue.max_consecutive
}

fn recoverable(
    custody: &ObserverCustody,
    boot: &str,
    current_instance: &str,
    owner_absent: bool,
    group_absent: bool,
) -> bool {
    if custody.boot != boot {
        return true;
    }
    let current_owner = custody.owner == std::process::id() && custody.instance == current_instance;
    (current_owner || owner_absent) && custody.pid.is_some() && group_absent
}

async fn observer_intent(store: &Store, task: &Id, marker: &Path) -> Result<ObserverCustody> {
    let boot = boot_identity().await?;
    if marker.exists() {
        let bytes = crate::private::read(marker, 8192)?;
        let old: ObserverCustody = serde_json::from_slice(&bytes)?;
        if old.version != 1
            || old.task != *task
            || old.owner <= 1
            || !xcb_core::hex64(&old.host)
            || uuid::Uuid::parse_str(&old.boot).is_err()
            || old.pid.is_some_and(|pid| pid <= 1)
        {
            return Err(Error::CleanupUnproven);
        }
        let owner_absent = crate::os::process_exists(old.owner) == Some(false);
        let group_absent = old
            .pid
            .is_some_and(|pid| crate::process::prove_process_group_absent(pid).is_ok());
        if !recoverable(&old, &boot, store.instance(), owner_absent, group_absent)
            || crate::private::read(marker, 8192)? != bytes
        {
            return Err(Error::CleanupUnproven);
        }
        std::fs::remove_file(marker).map_err(|_| Error::CleanupUnproven)?;
    }
    Ok(ObserverCustody {
        version: 1,
        task: task.clone(),
        owner: std::process::id(),
        instance: store.instance().into(),
        host: crate::process::host_identity()?.1,
        boot,
        pid: None,
    })
}

async fn api(store: &Store, task: &Id, endpoint: &str, projection: &str) -> Result<Value> {
    let gh = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
        .into_iter()
        .find_map(|path| xcb_core::canonical(path).ok())
        .ok_or(Error::Unavailable("trusted GitHub CLI is unavailable"))?;
    let stamp = crate::os::lstat(&gh)?;
    if !stamp.owned || !stamp.unshared_write {
        return Err(Error::PrivateState);
    }
    let home = xcb_core::canonical(PathBuf::from(
        std::env::var_os("HOME").ok_or(Error::PrivateState)?,
    ))?;
    let directory = crate::private::directory(&store.root().join("completion-observers"))?;
    let marker = directory.join(format!("{task}.json"));
    let mut custody = observer_intent(store, task, &marker).await?;
    let intent = serde_json::to_vec(&custody)?;
    crate::private::create(&marker, &intent)?;
    let mut command = Command::new(gh);
    command
        .args([
            "api",
            "--method",
            "GET",
            "--hostname",
            "github.com",
            endpoint,
            "--jq",
            projection,
        ])
        .env_clear()
        .env("HOME", home)
        .env(
            "PATH",
            "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
        )
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    let (_owner, cancel) = watch::channel(false);
    let outcome = crate::process::capture_supervised(
        command,
        64 * 1024,
        Duration::from_secs(5),
        cancel,
        |pid| {
            custody.pid = Some(pid);
            crate::private::replace(&marker, &serde_json::to_vec(&custody)?, &digest(&intent))
        },
    )
    .await;
    match outcome {
        crate::process::CaptureOutcome::Unproven => Err(Error::CleanupUnproven),
        outcome => {
            std::fs::remove_file(marker).map_err(|_| Error::CleanupUnproven)?;
            match outcome {
                crate::process::CaptureOutcome::Joined(Ok(bytes)) => {
                    Ok(serde_json::from_slice(&bytes)?)
                }
                _ => Err(Error::Unavailable(
                    "GitHub status query unavailable; bounded observer retry retained",
                )),
            }
        }
    }
}

async fn observe(
    store: &Store,
    task: &ManagedTask,
    target: &PrWatch,
) -> Result<(bool, &'static str)> {
    target.validate()?;
    if workspace::github_origin(Path::new(&task.workspace)).as_deref()
        != Some(target.repository.as_str())
    {
        return Err(Error::Conflict("completion PR origin changed"));
    }
    let config = Config::load(store.root())?.0;
    let session = task
        .session
        .as_ref()
        .and_then(|id| store.session(id).ok().flatten())
        .filter(|session| {
            session.workspace == task.workspace
                && session.managed_task.as_ref() == Some(&task.id)
                && session.requirements.native_execution
        })
        .ok_or(Error::Unavailable(
            "completion observer requires the native managed task session",
        ))?;
    let scope = config
        .native_execution
        .scope(Path::new(&task.workspace), session.model.provider)
        .filter(|scope| scope.github_credentials)
        .ok_or(Error::Unavailable(
            "completion observer requires this task provider's GitHub workspace grant",
        ))?;
    crate::native_backend::validate_scope(scope, store.root())?;
    let pull = api(
        store,
        &task.id,
        &format!("repos/{}/pulls/{}", target.repository, target.number),
        "{number,baseRepo:.base.repo.full_name,head:.head.sha,state,merged}",
    )
    .await?;
    // Never query status for a changed or completed PR under its old head.
    if pull["number"].as_u64() == Some(target.number)
        && pull["baseRepo"].as_str() == Some(target.repository.as_str())
        && (pull["head"].as_str() != Some(target.head.as_str())
            || pull["merged"] == true
            || pull["state"] != "open")
    {
        return response(
            target,
            &pull,
            &json!({"total":0,"runs":[]}),
            &json!({"total":0,"state":"success"}),
        );
    }
    let checks = api(
        store,
        &task.id,
        &format!(
            "repos/{}/commits/{}/check-runs?per_page=100",
            target.repository, target.head
        ),
        "{total:.total_count,runs:[.check_runs[]|{status,conclusion}]}",
    )
    .await?;
    let status = api(
        store,
        &task.id,
        &format!(
            "repos/{}/commits/{}/status?per_page=100",
            target.repository, target.head
        ),
        "{total:.total_count,state}",
    )
    .await?;
    response(target, &pull, &checks, &status)
}

impl ManagedStore {
    /// One bounded observer per supervisor pass; never a provider task.
    pub(super) async fn tick_completion_reviews(&self, store: &Store, now: u64) -> Result<()> {
        let candidate = self.active_tasks(128)?.into_iter().find(|task| {
            task.state == TaskState::Queued
                && task.completion_review.as_ref().is_some_and(|review| {
                    review.next_review_at_ms <= now
                        && review.pr.as_ref().is_some_and(|pr| pr.waiting)
                })
        });
        let Some(task) = candidate else {
            return Ok(());
        };
        if !self
            .herd_policy_in(&task.workspace)?
            .is_some_and(|policy| policy.enabled && policy.expires_at_ms > now)
            || self.project_dispatch_block(&task)?.is_some()
        {
            return Ok(());
        }
        let mut next = task.clone();
        let review = next.completion_review.as_mut().expect("selected review");
        if now >= review.wait_deadline_ms {
            next.state = TaskState::NeedsInput;
            next.attention = Some(State::NeedsAnswer);
            next.detail =
                "completion observer horizon exhausted; requested delivery remains incomplete"
                    .into();
        } else {
            let pr = review.pr.as_mut().expect("selected PR");
            match observe(store, &task, pr).await {
                Ok((waiting, status)) => {
                    pr.waiting = waiting;
                    pr.status = status.into();
                    pr.observed_at_ms = Some(now_ms());
                }
                Err(Error::CleanupUnproven) => {
                    pr.status = "observer cleanup unproven; waiting for exact process exit or changed boot proof".into();
                }
                Err(Error::Unavailable(reason))
                    if reason.starts_with("completion observer requires")
                        || reason == "trusted GitHub CLI is unavailable" =>
                {
                    next.state = TaskState::NeedsInput;
                    next.attention = Some(State::NeedsAction);
                    next.detail = reason.into();
                }
                Err(Error::Conflict(reason)) if reason == "completion PR origin changed" => {
                    next.state = TaskState::NeedsInput;
                    next.attention = Some(State::NeedsAction);
                    next.detail = reason.into();
                }
                Err(_) => {
                    pr.status = "GitHub observation unavailable; bounded retry retained".into();
                }
            }
            pr.polls = pr.polls.saturating_add(1);
            review.next_review_at_ms =
                now_ms().saturating_add((60_000u64 << pr.polls.min(4)).min(900_000));
            if next.state == TaskState::Queued {
                next.detail = pr.status.clone();
                if !pr.waiting {
                    review.next_review_at_ms = now_ms();
                    let config = Config::load(store.root())?.0;
                    if !worker_budget_available(&task, &config) {
                        next.state = TaskState::NeedsInput;
                        next.attention = Some(State::NeedsAnswer);
                        next.detail = "PR checks settled, but productive continuation budget is exhausted; delivery remains incomplete".into();
                    }
                    review.productive_since_ms = now_ms();
                    let observed = format!(
                        "Host read-only observation at {}: {}. Repository {}, PR {}, observed target head {}. This does not authorize merging or waive any required gates.",
                        now_ms(),
                        pr.status,
                        pr.repository,
                        pr.number,
                        pr.head
                    );
                    next.next_prompt = format!("{}\n{observed}", completion::prompt(&task, review));
                }
            }
        }
        next.revision += 1;
        next.updated_at_ms = now_ms();
        self.transition(&task, next, None).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_head_observer_waits_and_wakes_without_delivery_authority() {
        let target = PrWatch {
            repository: "owner/repo".into(),
            number: 1,
            head: "a".repeat(40),
            waiting: true,
            polls: 0,
            observed_at_ms: None,
            status: "pending".into(),
        };
        let mut pull = json!({"number":1,"baseRepo":"owner/repo","head":target.head,"state":"open","merged":false});
        let pending = json!({"total":1,"runs":[{"status":"in_progress","conclusion":null}]});
        let status = json!({"total":0,"state":"pending"});
        assert!(response(&target, &pull, &pending, &status).unwrap().0);
        pull["head"] = json!("b".repeat(40));
        assert!(!response(&target, &pull, &pending, &status).unwrap().0);
        pull["baseRepo"] = json!("foreign/repo");
        assert!(response(&target, &pull, &pending, &status).is_err());
        assert!(!repository("owner/repo/../../other"));
    }
    #[test]
    fn completion_observer_recovery_requires_boot_or_exact_absence_proof() {
        let mut old = ObserverCustody {
            version: 1,
            task: new_id("task"),
            owner: std::process::id(),
            instance: "old".into(),
            host: "a".repeat(64),
            boot: "boot-a".into(),
            pid: Some(42),
        };
        assert!(!recoverable(&old, "boot-a", "current", false, true));
        assert!(!recoverable(&old, "boot-a", "current", true, false));
        assert!(recoverable(&old, "boot-a", "current", true, true));
        assert!(recoverable(&old, "boot-b", "current", false, false));
        old.pid = None;
        assert!(!recoverable(&old, "boot-a", "current", true, true));
        assert!(recoverable(&old, "boot-b", "current", false, false));
    }
    #[tokio::test]
    async fn completion_watch_binding_and_productive_budget_do_not_expand_scope() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let work = private::directory(&root.join("work")).unwrap();
        std::fs::create_dir(work.join(".git")).unwrap();
        std::fs::write(
            work.join(".git/config"),
            "[remote \"origin\"]\n url = https://github.com/o/r.git\n",
        )
        .unwrap();
        let report = format!(
            "WAIT_PR pr=https://github.com/o/r/pull/1 head={}",
            "a".repeat(40)
        );
        assert!(request(&report, work.to_str().unwrap()).is_some());
        assert!(
            request(
                &report.replace("github.com/o/r", "github.com/foreign/repo"),
                work.to_str().unwrap()
            )
            .is_none()
        );
        std::fs::write(
            work.join(".git/config"),
            "[remote \"origin\"]\n url = https://other.example/o/r.git\n",
        )
        .unwrap();
        assert!(request(&report, work.to_str().unwrap()).is_none());
        let managed = ManagedStore::open(&root.join("state")).unwrap();
        let chat = managed.create_conversation(&work).await.unwrap().id;
        let mut task = managed
            .enqueue_backlog(&chat, new_id("test"), "Merge the fix".into(), false, 5)
            .await
            .unwrap();
        assert!(worker_budget_available(&task, &Config::default()));
        task.attempts = task.max_attempts;
        assert!(!worker_budget_available(&task, &Config::default()));
    }
}
