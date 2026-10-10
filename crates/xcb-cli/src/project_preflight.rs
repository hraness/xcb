//! Local setup checks. This module never creates a grant, task, or schedule.
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;
use xcb_runtime::{Result, managed_program::AdmittedProgram};

const GIT_INDEX_CAPTURE_LIMIT: usize = 64 * 1024 * 1024;

async fn git_bytes(workspace: &Path, args: &[&str], input: Option<&[u8]>) -> Option<Vec<u8>> {
    git_bytes_bounded(workspace, args, input, 1024 * 1024).await
}

async fn git_bytes_bounded(
    workspace: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    max_output: usize,
) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    // Unit fixtures own their repository config; never inherit host filters.
    #[cfg(test)]
    command
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(workspace)
        .args(args);
    match input {
        Some(input) => xcb_runtime::process::capture_with_input(
            command,
            input,
            max_output,
            Duration::from_secs(5),
        )
        .await
        .ok(),
        None => xcb_runtime::process::capture(command, max_output, Duration::from_secs(5))
            .await
            .ok(),
    }
}

async fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    git_bytes(workspace, args, None)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
}

async fn safe_status(workspace: &Path) -> Option<String> {
    let entries = git_bytes_bounded(
        workspace,
        &["ls-files", "--stage", "-z"],
        None,
        GIT_INDEX_CAPTURE_LIMIT,
    )
    .await?;
    let mut paths = Vec::new();
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        // Nested repositories have independent configuration and require a
        // separate inspection. Never invoke their status helpers implicitly.
        if entry.starts_with(b"160000 ") {
            return None;
        }
        let tab = entry.iter().position(|byte| *byte == b'\t')?;
        paths.extend_from_slice(&entry[tab + 1..]);
        paths.push(0);
    }
    let attrs = git_bytes_bounded(
        workspace,
        &["check-attr", "-z", "--stdin", "filter"],
        Some(&paths),
        GIT_INDEX_CAPTURE_LIMIT,
    )
    .await?;
    let fields = attrs.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.last().is_none_or(|field| !field.is_empty()) || (fields.len() - 1) % 3 != 0 {
        return None;
    }
    if fields[..fields.len() - 1]
        .chunks_exact(3)
        .any(|field| field[1] != b"filter" || !matches!(field[2], b"unspecified" | b"unset"))
    {
        return None;
    }
    git(
        workspace,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    )
    .await
}

fn required_file(workspace: &Path, path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    workspace
        .join(path)
        .canonicalize()
        .ok()
        .filter(|resolved| resolved.starts_with(workspace) && resolved.is_file())
}

pub(super) async fn inspect(
    workspace: &Path,
    program: &AdmittedProgram,
    required_files: &[PathBuf],
    expected_revision: Option<&str>,
    task_budget: Option<u32>,
) -> Result<Value> {
    let workspace = workspace.canonicalize()?;
    let mut blockers = Vec::new();
    let revision = git(&workspace, &["rev-parse", "--verify", "HEAD"]).await;
    let root = git(&workspace, &["rev-parse", "--show-toplevel"]).await;
    let branch = git(&workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await;
    let status = match root.as_deref() {
        Some(root) => safe_status(Path::new(root)).await,
        None => None,
    };
    if !workspace.is_dir() || root.is_none() || revision.is_none() || status.is_none() {
        blockers.push("Git inspection failed, exceeded a capture limit (64 MiB for index/attributes; 1 MiB for other Git output), or timed out; require a readable committed checkout without filtered tracked files or submodules".to_owned());
    }
    let dirty = status.as_ref().map(|s| !s.is_empty());
    if dirty == Some(true) {
        blockers.push("workspace has tracked or untracked changes; review them before enabling unattended work".to_owned());
    }
    if expected_revision.is_some_and(|expected| revision.as_deref() != Some(expected)) {
        blockers.push("HEAD does not match the expected full commit hash".to_owned());
    }
    let files = required_files.iter().map(|path| {
        let content = if let Some(resolved) = required_file(&workspace, path) {
            let mut bytes = Vec::new();
            // Inputs are reviewable text artifacts, not unbounded datasets.
            std::fs::File::open(resolved).ok()
                .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok())
                .filter(|_| bytes.len() <= 1024 * 1024)
                .map(|_| bytes)
        } else { None };
        let present = content.is_some();
        if !present {
            blockers.push(format!("required file is missing, unreadable, larger than 1 MiB, or outside the workspace: {}", path.display()));
        }
        json!({"path": path, "ready": present, "sha256": content.map(xcb_runtime::digest)})
    }).collect::<Vec<_>>();
    let agent_cells = program.manifest["cells"].as_array().map_or(0, |cells| {
        cells.iter().filter(|cell| cell["kind"] == "agent").count() as u32
    });
    let estimated_cycles = task_budget
        .zip((agent_cells > 0).then_some(agent_cells))
        .map(|(budget, calls)| budget / calls);
    let mut warnings = Vec::new();
    if estimated_cycles == Some(0) {
        warnings.push(
            "remaining child-task budget is smaller than one pass through all declared agent cells"
                .to_owned(),
        );
    }
    Ok(json!({
        "ready": blockers.is_empty(), "workspace": workspace,
        "gitRoot": root, "revision": revision, "branch": branch, "dirty": dirty,
        "expectedRevision": expected_revision, "requiredFiles": files,
        "manifestDigest": program.manifest_digest, "inputsDigest": program.inputs_digest,
        "managedCallCeiling": program.managed_calls, "declaredAgentCells": agent_cells,
        "remainingChildTaskBudget": task_budget, "estimatedFullCycles": estimated_cycles,
        "budgetEstimateBasis": "assumes one child per declared agent cell; not a guaranteed cycle count; excludes other automatic work, expiry and failures",
        "blockers": blockers, "warnings": warnings,
        "limitations": "local snapshot only; does not verify provider access, disk capacity, grants, execution, delivery, or future workspace changes"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        async fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("xcb-preflight-{}", xcb_runtime::new_id("test")));
            std::fs::create_dir_all(&root).unwrap();
            let fixture = Self(root.canonicalize().unwrap());
            assert!(git(&fixture.0, &["init", "--quiet"]).await.is_some());
            std::fs::write(fixture.0.join("plan.md"), "plan").unwrap();
            assert!(git(&fixture.0, &["add", "plan.md"]).await.is_some());
            assert!(
                git(
                    &fixture.0,
                    &[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "core.hooksPath=/dev/null",
                        "commit",
                        "--quiet",
                        "-m",
                        "fixture"
                    ]
                )
                .await
                .is_some()
            );
            fixture
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn program() -> AdmittedProgram {
        AdmittedProgram::admit_managed(
            serde_json::from_str(include_str!(
                "../../../examples/xcb-north-star-controller.algal.json"
            ))
            .unwrap(),
            json!({}),
            8,
        )
        .unwrap()
    }
    #[tokio::test]
    async fn preflight_checks_exact_revision_inputs_and_real_cell_budget() {
        let fixture = Fixture::new().await;
        let revision = git(&fixture.0, &["rev-parse", "HEAD"]).await.unwrap();
        let report = inspect(
            &fixture.0,
            &program(),
            &["plan.md".into()],
            Some(&revision),
            Some(100),
        )
        .await
        .unwrap();
        assert_eq!(report["ready"], true);
        assert_eq!(
            report["requiredFiles"][0]["sha256"],
            xcb_runtime::digest(b"plan")
        );
        assert_eq!(report["declaredAgentCells"], 5);
        assert_eq!(report["managedCallCeiling"], 8);
        assert_eq!(report["estimatedFullCycles"], 20);
        let failed = inspect(
            &fixture.0,
            &program(),
            &["missing.md".into()],
            Some("wrong"),
            Some(4),
        )
        .await
        .unwrap();
        assert_eq!(failed["ready"], false);
        assert_eq!(failed["blockers"].as_array().unwrap().len(), 2);
        std::fs::write(fixture.0.join("unrelated.txt"), "preserve").unwrap();
        assert_eq!(
            inspect(&fixture.0, &program(), &[], None, None)
                .await
                .unwrap()["dirty"],
            true
        );
        assert!(fixture.0.join("unrelated.txt").exists());
    }
    #[tokio::test]
    async fn preflight_rejects_required_paths_outside_workspace() {
        let fixture = Fixture::new().await;
        assert!(required_file(&fixture.0, Path::new("../outside")).is_none());
        assert!(required_file(&fixture.0, &fixture.0.join("plan.md")).is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", fixture.0.join("escape")).unwrap();
            assert!(required_file(&fixture.0, Path::new("escape")).is_none());
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn preflight_accepts_large_clean_index_but_still_rejects_unsafe_worktrees() {
        let fixture = Fixture::new().await;
        // Shared long prefixes keep the fixture small while both Git captures
        // exceed the ordinary 1 MiB cap.
        let directory = fixture
            .0
            .join("a".repeat(160))
            .join("b".repeat(160))
            .join("c".repeat(160));
        std::fs::create_dir_all(&directory).unwrap();
        for i in 0..1550 {
            std::fs::write(
                directory.join(format!("{i:04}{}.txt", "f".repeat(175))),
                b"x",
            )
            .unwrap();
        }
        assert!(git(&fixture.0, &["add", "."]).await.is_some());
        assert!(
            git(
                &fixture.0,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "--quiet",
                    "-m",
                    "large index"
                ]
            )
            .await
            .is_some()
        );
        let entries = git_bytes_bounded(
            &fixture.0,
            &["ls-files", "--stage", "-z"],
            None,
            GIT_INDEX_CAPTURE_LIMIT,
        )
        .await
        .unwrap();
        assert!(
            entries.len() > 1024 * 1024,
            "ls-files was only {} bytes",
            entries.len()
        );
        let paths = entries
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .flat_map(|entry| {
                let tab = entry.iter().position(|byte| *byte == b'\t').unwrap();
                entry[tab + 1..].iter().copied().chain([0])
            })
            .collect::<Vec<_>>();
        let attrs = git_bytes_bounded(
            &fixture.0,
            &["check-attr", "-z", "--stdin", "filter"],
            Some(&paths),
            GIT_INDEX_CAPTURE_LIMIT,
        )
        .await
        .unwrap();
        assert!(
            attrs.len() > 1024 * 1024,
            "check-attr was only {} bytes",
            attrs.len()
        );
        let clean = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(clean["ready"], true, "{clean}");
        assert_eq!(clean["dirty"], false);

        std::fs::write(fixture.0.join(".gitattributes"), "*.txt filter=probe\n").unwrap();
        let filtered = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(filtered["ready"], false);
        assert_eq!(filtered["dirty"], Value::Null);
        std::fs::remove_file(fixture.0.join(".gitattributes")).unwrap();

        let revision = git(&fixture.0, &["rev-parse", "HEAD"]).await.unwrap();
        let gitlink = format!("160000,{revision},nested-repo");
        assert!(
            git(
                &fixture.0,
                &["update-index", "--add", "--cacheinfo", &gitlink]
            )
            .await
            .is_some()
        );
        let submodule = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(submodule["ready"], false);
        assert_eq!(submodule["dirty"], Value::Null);
        assert!(
            git(
                &fixture.0,
                &["update-index", "--force-remove", "nested-repo"]
            )
            .await
            .is_some()
        );

        std::fs::write(fixture.0.join("plan.md"), "changed").unwrap();
        let dirty = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(dirty["ready"], false);
        assert_eq!(dirty["dirty"], true);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn preflight_does_not_run_git_hooks_or_filters() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new().await;
        let hook = fixture.0.join(".git/probe-hook");
        let marker = fixture.0.join(".git/hook-ran");
        std::fs::write(&hook, "#!/bin/sh\ntouch .git/hook-ran\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            git(
                &fixture.0,
                &["config", "core.fsmonitor", hook.to_str().unwrap()]
            )
            .await
            .is_some()
        );
        let report = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(report["ready"], true);
        assert!(!marker.exists());
        assert!(
            git(
                &fixture.0,
                &["config", "filter.probe.clean", hook.to_str().unwrap()]
            )
            .await
            .is_some()
        );
        // Unused global/local filter registrations do not block this checkout.
        assert_eq!(
            inspect(&fixture.0, &program(), &[], None, None)
                .await
                .unwrap()["ready"],
            true
        );
        std::fs::write(fixture.0.join(".gitattributes"), "*.md filter=probe\n").unwrap();
        std::fs::write(fixture.0.join("plan.md"), "changed").unwrap();
        let report = inspect(&fixture.0, &program(), &[], None, None)
            .await
            .unwrap();
        assert_eq!(report["ready"], false);
        assert_eq!(report["dirty"], Value::Null);
        assert!(!marker.exists());
    }
}
