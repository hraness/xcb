//! Explicit, local Wordcell memory exchange. A host binds one trusted CLI and
//! vault to a conversation. Search never enables semantic, history, graph, or
//! hosted lanes. Promotion exports only the supplied note, with provenance;
//! it never reads or uploads conversation transcripts.
//!
//! Pins detect launcher/interpreter drift, not changes throughout an imported
//! package graph. The host explicitly trusts its installed Wordcell distribution.

use crate::{Error, Result, digest, private, process};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::watch};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_NOTE_BYTES: usize = 8 * 1024;
const MAX_RECEIPT_BYTES: usize = 8 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutablePin {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WordcellConfig {
    pub executable: ExecutablePin,
    pub interpreter: Option<ExecutablePin>,
    pub vault: PathBuf,
    pub vault_device: u64,
    pub vault_inode: u64,
}

/// `summary` is a user-selected/authored note, not an automatically copied task
/// summary. Source identity is attached separately and cannot select a path.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Promotion {
    pub task_id: String,
    pub conversation_id: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionStatus {
    Prepared,
    Completed,
    NotStarted,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotionReceipt {
    pub request_digest: String,
    pub note_id: String,
    pub status: PromotionStatus,
    pub process_id: Option<u32>,
    pub cleanup_proven: bool,
    pub note_revision: Option<String>,
}

fn unavailable() -> Error {
    Error::Unavailable(
        "Wordcell requires an unchanged trusted executable and canonical owned vault",
    )
}

impl ExecutablePin {
    fn admit(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(unavailable());
        }
        let path = path.canonicalize()?;
        let sha256 = process::executable_digest(&path)?;
        Ok(Self { path, sha256 })
    }

    fn verify(&self) -> Result<()> {
        if !self.path.is_absolute()
            || self.path.canonicalize()? != self.path
            || process::executable_digest(&self.path)? != self.sha256
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

fn interpreter(executable: &Path) -> Result<Option<ExecutablePin>> {
    let mut prefix = String::new();
    let mut bytes = Vec::new();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(executable)?;
    if !file.metadata()?.is_file() {
        return Err(unavailable());
    }
    file.take(512).read_to_end(&mut bytes)?;
    if !bytes.starts_with(b"#!") {
        return Ok(None);
    }
    // Only a single interpreter or the shipped Wordcell `env bun` launcher is
    // admitted. Resolve the latter once from host PATH, then pin and launch that
    // absolute interpreter directly; subsequent PATH changes cannot redirect it.
    prefix.push_str(
        std::str::from_utf8(bytes.split(|b| *b == b'\n').next().unwrap_or(&[]))
            .map_err(|_| unavailable())?,
    );
    let words: Vec<_> = prefix[2..].split_whitespace().collect();
    let path = match words.as_slice() {
        [path] if Path::new(path).is_absolute() => PathBuf::from(path),
        ["/usr/bin/env", "bun"] => {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|p| p.is_absolute())
                .map(|p| p.join("bun"))
                .find(|p| p.is_file())
                .ok_or(Error::Unavailable(
                    "Wordcell's Bun interpreter is not on the trusted host PATH",
                ))?
        }
        _ => {
            return Err(Error::Unavailable(
                "unsupported Wordcell executable interpreter",
            ));
        }
    };
    ExecutablePin::admit(&path).map(Some)
}

fn vault_metadata(vault: &Path) -> Result<fs::Metadata> {
    if !vault.is_absolute() || vault.canonicalize()? != vault {
        return Err(unavailable());
    }
    let metadata = fs::symlink_metadata(vault)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(unavailable());
    }
    Ok(metadata)
}

impl WordcellConfig {
    pub fn admit(executable: &Path, vault: &Path) -> Result<Self> {
        if !vault.is_absolute() {
            return Err(unavailable());
        }
        let vault = vault.canonicalize()?;
        let metadata = vault_metadata(&vault)?;
        let executable = ExecutablePin::admit(executable)?;
        let interpreter = interpreter(&executable.path)?;
        Ok(Self {
            executable,
            interpreter,
            vault,
            vault_device: metadata.dev(),
            vault_inode: metadata.ino(),
        })
    }

    pub fn verify(&self) -> Result<()> {
        self.executable.verify()?;
        let metadata = vault_metadata(&self.vault)?;
        if metadata.dev() != self.vault_device || metadata.ino() != self.vault_inode {
            return Err(unavailable());
        }
        if let Some(interpreter) = &self.interpreter {
            interpreter.verify()?;
        }
        Ok(())
    }

    fn command(&self) -> Result<Command> {
        self.verify()?;
        let mut command = Command::new(
            self.interpreter
                .as_ref()
                .unwrap_or(&self.executable)
                .path
                .clone(),
        );
        if self.interpreter.is_some() {
            command.arg(&self.executable.path);
        }
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "en_US.UTF-8")
            .env("NO_COLOR", "1")
            .env("HRANESS_SUPPORT_AUDIENCE", "off")
            .current_dir(&self.vault);
        Ok(command)
    }

    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        if query.trim().is_empty()
            || query.len() > 1024
            || !(1..=16).contains(&limit)
            || query.starts_with('-')
        {
            return Err(Error::Unavailable(
                "Wordcell search requires a non-option query up to 1024 bytes and limit 1..16",
            ));
        }
        let mut command = self.command()?;
        command
            .args(["search", query, "--root"])
            .arg(&self.vault)
            .args([
                "--mode",
                "exact",
                "--no-history",
                "--no-graph",
                "--limit",
                &limit.to_string(),
                "--json",
            ]);
        // Search has no mutating lane. Supervision still joins its process group
        // and pipes on cancellation, output overflow, and deadline expiration.
        match process::capture_supervised(command, MAX_OUTPUT_BYTES, DEADLINE, cancel, |_| Ok(()))
            .await
        {
            process::CaptureOutcome::NeverStarted(error) => Err(error),
            process::CaptureOutcome::Joined(result) => {
                let bytes = result?;
                let value: Value = serde_json::from_slice(&bytes)?;
                if !value.is_object() {
                    return Err(Error::Protocol(
                        "Wordcell search returned an invalid result",
                    ));
                }
                Ok(value)
            }
            process::CaptureOutcome::Unproven => Err(Error::CleanupUnproven),
        }
    }

    /// A stable request pins the exact note, source IDs, vault, and executable.
    /// Wordcell's no-clobber `note create` makes retry of *this same request*
    /// idempotent. No timeout or nonzero exit is reported as a successful write.
    pub async fn promote(
        &self,
        promotion: &Promotion,
        custody_dir: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<PromotionReceipt> {
        self.promote_with_deadline(promotion, custody_dir, cancel, DEADLINE)
            .await
    }

    async fn promote_with_deadline(
        &self,
        promotion: &Promotion,
        custody_dir: &Path,
        cancel: watch::Receiver<bool>,
        deadline: Duration,
    ) -> Result<PromotionReceipt> {
        self.verify()?;
        for id in [&promotion.task_id, &promotion.conversation_id] {
            xcb_core::Id::new(id.clone())?;
        }
        if promotion.summary.trim().is_empty() || promotion.summary.len() > MAX_NOTE_BYTES {
            return Err(Error::Unavailable(
                "Wordcell promotion requires an explicit note of 1..8192 bytes",
            ));
        }
        if *cancel.borrow() {
            return Err(Error::Unavailable(
                "Wordcell promotion cancelled before launch",
            ));
        }
        let directory = private::directory(custody_dir)?;
        let request_digest = digest(serde_json::to_vec(&json!({
            "contract":"xcb.wordcell-promotion.v1", "config":self, "promotion":promotion
        }))?);
        // Root-level IDs also work in ordinary Markdown folders; Wordcell
        // intentionally never creates missing parent directories for authors.
        let note_id = format!("xcb-{request_digest}");
        let lock_path = directory.join(format!("{request_digest}.lock"));
        match private::create(&lock_path, b"") {
            Ok(()) => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        let lock = private::open_file(&lock_path, 0)?;
        lock.try_lock()
            .map_err(|_| Error::Conflict("this Wordcell promotion is already running"))?;
        let lock = private::ExclusiveLock::held(lock);
        private::same_file(&lock_path, &lock)?;
        let receipt_path = directory.join(format!("{request_digest}.json"));
        let mut old = match private::read(&receipt_path, MAX_RECEIPT_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(bytes) = &old {
            let receipt: PromotionReceipt = serde_json::from_slice(bytes)?;
            if receipt.request_digest != request_digest || receipt.note_id != note_id {
                return Err(Error::Conflict("Wordcell promotion receipt changed"));
            }
            if receipt.status == PromotionStatus::Completed {
                return Ok(receipt);
            }
            if !receipt.cleanup_proven {
                let pid = receipt.process_id.ok_or(Error::Unavailable(
                    "Wordcell promotion has an unresolved launch; inspect its receipt before recovery",
                ))?;
                process::prove_process_group_absent(pid)?;
            }
        }
        let body_path = directory.join(format!("{request_digest}.md"));
        let body = format!(
            "{}\n\n---\nSource: xcb conversation `{}`, task `{}`.\nPromotion request: `{}`.\n",
            promotion.summary.trim_end(),
            promotion.conversation_id,
            promotion.task_id,
            request_digest,
        );
        match private::create(&body_path, body.as_bytes()) {
            Ok(()) => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if private::read(&body_path, MAX_NOTE_BYTES + 1024)? != body.as_bytes() {
                    return Err(Error::Conflict("Wordcell promotion note changed"));
                }
            }
            Err(error) => return Err(error),
        }
        let mut receipt = PromotionReceipt {
            request_digest,
            note_id,
            status: PromotionStatus::Prepared,
            process_id: None,
            cleanup_proven: false,
            note_revision: None,
        };
        let mut command = self.command()?;
        command
            .args(["note", "create", &receipt.note_id, "--title"])
            .arg(format!("xcb task {}", promotion.task_id))
            .args(["--type", "note", "--tag", "xcb", "--body-file"])
            .arg(&body_path)
            .arg("--root")
            .arg(&self.vault)
            .arg("--json");
        save_receipt(&receipt_path, &receipt, &mut old)?;
        let outcome =
            process::capture_supervised(command, MAX_OUTPUT_BYTES, deadline, cancel, |pid| {
                receipt.process_id = Some(pid);
                save_receipt(&receipt_path, &receipt, &mut old)
            })
            .await;
        match outcome {
            process::CaptureOutcome::NeverStarted(_) => {
                receipt.status = PromotionStatus::NotStarted;
                receipt.cleanup_proven = true;
            }
            process::CaptureOutcome::Joined(result) => {
                receipt.cleanup_proven = true;
                receipt.status = PromotionStatus::Uncertain;
                if let Ok(bytes) = result
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && value["path"].as_str() == Some(format!("{}.md", receipt.note_id).as_str())
                    && let Some(revision) = value["revision"].as_str()
                    && revision.strip_prefix("sha256:").is_some_and(|hash| {
                        hash.len() == 64
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                {
                    receipt.status = PromotionStatus::Completed;
                    receipt.note_revision = Some(revision.to_owned());
                }
            }
            process::CaptureOutcome::Unproven => {
                receipt.status = PromotionStatus::Uncertain;
            }
        }
        save_receipt(&receipt_path, &receipt, &mut old)?;
        Ok(receipt)
    }
}

fn save_receipt(path: &Path, receipt: &PromotionReceipt, old: &mut Option<Vec<u8>>) -> Result<()> {
    let bytes = serde_json::to_vec(receipt)?;
    match old {
        Some(previous) => private::replace(path, &bytes, &digest(previous))?,
        None => private::create(path, &bytes)?,
    }
    *old = Some(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(script: &str) -> (tempfile::TempDir, WordcellConfig) {
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let executable = root.path().join("wordcell");
        fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let config = WordcellConfig::admit(&executable, &vault).unwrap();
        (root, config)
    }

    fn promotion() -> Promotion {
        Promotion {
            task_id: "task_one".into(),
            conversation_id: "conversation_one".into(),
            summary: "The explicit durable decision.".into(),
        }
    }

    #[tokio::test]
    async fn exact_search_has_no_semantic_history_graph_or_shell_argument_lane() {
        let (_root, config) = fixture(
            r#"
test "$1" = search || exit 2
test "$2" = 'a query; $(exit 42)' || exit 3
test "$5" = --mode && test "$6" = exact || exit 4
test "$7" = --no-history && test "$8" = --no-graph || exit 5
test "$9" = --limit && test "${10}" = 3 && test "${11}" = --json || exit 6
printf '{"results":[]}'
"#,
        );
        let (_sender, cancel) = watch::channel(false);
        assert_eq!(
            config
                .search("a query; $(exit 42)", 3, cancel.clone())
                .await
                .unwrap(),
            json!({"results":[]})
        );
        assert!(config.search("--rerank", 3, cancel).await.is_err());
    }

    #[tokio::test]
    async fn promotion_records_provenance_and_replays_one_completed_request() {
        let (root, config) = fixture(
            r#"
test "$1" = note && test "$2" = create || exit 2
test "${10}" = --body-file || exit 3
test -f "${11}" || exit 4
printf '{"changed":true,"path":"%s.md","revision":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' "$3"
"#,
        );
        let custody = root.path().canonicalize().unwrap().join("custody");
        let (_sender, cancel) = watch::channel(false);
        let first = config
            .promote(&promotion(), &custody, cancel.clone())
            .await
            .unwrap();
        assert_eq!(first.status, PromotionStatus::Completed);
        assert!(first.cleanup_proven);
        let body = private::read(
            &custody.join(format!("{}.md", first.request_digest)),
            MAX_NOTE_BYTES + 1024,
        )
        .unwrap();
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("The explicit durable decision."));
        assert!(body.contains("conversation_one") && body.contains("task_one"));
        let again = config
            .promote(&promotion(), &custody, cancel)
            .await
            .unwrap();
        assert_eq!(again.process_id, first.process_id);
    }

    #[tokio::test]
    async fn timed_out_write_is_uncertain_with_joined_process_evidence() {
        let (root, config) = fixture("exec /bin/sleep 30");
        let (_sender, cancel) = watch::channel(false);
        let receipt = config
            .promote_with_deadline(
                &promotion(),
                &root.path().canonicalize().unwrap().join("custody"),
                cancel,
                Duration::from_millis(50),
            )
            .await
            .unwrap();
        assert_eq!(receipt.status, PromotionStatus::Uncertain);
        assert!(receipt.cleanup_proven);
        process::prove_process_group_absent(receipt.process_id.unwrap()).unwrap();
    }

    #[tokio::test]
    async fn ambiguous_success_and_oversized_search_output_are_rejected() {
        let (root, config) = fixture("printf '{\"ok\":true}'");
        let (_sender, cancel) = watch::channel(false);
        let receipt = config
            .promote(
                &promotion(),
                &root.path().canonicalize().unwrap().join("custody"),
                cancel.clone(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.status, PromotionStatus::Uncertain);
        assert!(receipt.cleanup_proven);
        let (_root, config) = fixture(
            "i=0; while [ \"$i\" -lt 2000 ]; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'; i=$((i+1)); done",
        );
        assert!(config.search("query", 1, cancel).await.is_err());
    }

    #[tokio::test]
    async fn prelaunch_cancellation_creates_no_promotion_receipt() {
        let (root, config) = fixture("printf '{}'");
        let custody = root.path().canonicalize().unwrap().join("custody");
        let (_sender, cancel) = watch::channel(true);
        assert!(
            config
                .promote(&promotion(), &custody, cancel)
                .await
                .is_err()
        );
        assert!(!custody.exists());
    }

    #[test]
    fn changed_executable_or_vault_identity_requires_reconfiguration() {
        let (root, config) = fixture("printf '{}'");
        fs::write(
            &config.executable.path,
            "#!/bin/sh\nprintf '{\"changed\":true}'",
        )
        .unwrap();
        assert!(config.verify().is_err());
        let (_root, mut config) = fixture("printf '{}'");
        config.vault_inode = config.vault_inode.wrapping_add(1);
        assert!(config.verify().is_err());
        assert!(WordcellConfig::admit(Path::new("relative"), root.path()).is_err());
    }
}
