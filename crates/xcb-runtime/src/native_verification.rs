use crate::{
    Error, Result,
    config::Config,
    native_backend::{self, NativeScope, ProviderQualification},
    now_ms, private, process, routing, runner,
    store::Store,
};
use serde_json::json;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::watch};
use xcb_core::{
    Provider,
    policy::EffectState,
    session::{Message, Role, State, TaskRequirements},
};

/// Re-run native checks the owner already passed once and that have since
/// lapsed (an xcb upgrade, an adopted provider build, or receipt age) for
/// providers a workspace grant still names. It never checks a provider the
/// owner has not, and never changes a grant. Returns what failed.
pub async fn requalify_lapsed(store: Arc<Store>) -> Vec<String> {
    let mut faults = Vec::new();
    let root = store.root().to_path_buf();
    let Ok((config, _)) = Config::load(&root) else {
        return faults;
    };
    let mut providers: Vec<(Provider, bool)> = Vec::new();
    for scope in &config.native_execution.scopes {
        for provider in &scope.providers {
            match providers.iter_mut().find(|(known, _)| known == provider) {
                Some((_, github)) => *github |= scope.github_credentials,
                None => providers.push((*provider, scope.github_credentials)),
            }
        }
    }
    if providers.is_empty() {
        return faults;
    }
    if native_backend::require_qualification(&root).is_err() {
        if !root.join("qualification/native-command.json").exists() {
            return faults;
        }
        if let Err(error) = crate::command_tool::qualify_native(&root).await {
            faults.push(format!("native command checks: {error}"));
            return faults;
        }
    }
    for (provider, github) in providers {
        if native_backend::require_provider_qualification(&root, provider, github).is_ok()
            || !root
                .join(format!("qualification/native-{provider}.json"))
                .exists()
        {
            continue;
        }
        let (_keep, cancel) = watch::channel(false);
        if let Err(error) = verify_for_account(store.clone(), provider, None, github, cancel).await
        {
            faults.push(format!("{provider} native checks: {error}"));
        }
    }
    faults
}

pub async fn verify(
    store: Arc<Store>,
    provider: Provider,
    github: bool,
    cancel: watch::Receiver<bool>,
) -> Result<ProviderQualification> {
    verify_for_account(store, provider, None, github, cancel).await
}

pub async fn verify_for_account(
    store: Arc<Store>,
    provider: Provider,
    account: Option<xcb_core::Id>,
    github: bool,
    cancel: watch::Receiver<bool>,
) -> Result<ProviderQualification> {
    if let Some(account) = &account
        && store.account(account)?.provider != provider
    {
        return Err(Error::Unavailable(
            "native verification account belongs to another provider",
        ));
    }
    native_backend::require_qualification(store.root())?;
    let mut lookup = Command::new("/usr/bin/xcrun");
    lookup
        .args(["--find", "git"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    let git = match process::capture_supervised(
        lookup,
        4096,
        Duration::from_secs(10),
        cancel.clone(),
        |_| Ok(()),
    )
    .await
    {
        process::CaptureOutcome::Joined(Ok(bytes)) => xcb_core::canonical(PathBuf::from(
            std::str::from_utf8(&bytes)
                .map_err(|_| Error::PrivateState)?
                .trim(),
        ))?,
        process::CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
        _ => return Err(Error::Unavailable("the native Git toolchain is not ready")),
    };
    let fixture = tempfile::Builder::new()
        .prefix("xcb-native-qualification-")
        .tempdir()?;
    let workspace = xcb_core::canonical(fixture.path())?;
    let mut read_only_roots = vec![
        git.parent()
            .and_then(|parent| parent.parent())
            .ok_or(Error::PrivateState)?
            .to_owned(),
    ];
    if github {
        read_only_roots.push(xcb_core::canonical("/opt/homebrew")?);
    }
    let scope = NativeScope {
        workspace: workspace.clone(),
        providers: vec![provider],
        github_credentials: github,
        read_only_roots,
        git_metadata: vec![],
        host_read: false,
    };
    let mut script = "set -eu; printf native-marker > native.txt; test \"$(cat native.txt)\" = native-marker; printf 'native-shell-ok\\n'; curl --silent --show-error --fail --max-time 20 --output /dev/null https://api.github.com/meta; printf 'native-dns-https-ok\\n'; git init --quiet .; git add native.txt; git -c user.name=XCB -c user.email=native@example.invalid commit --quiet -m 'native qualification'; test \"$(git rev-list --count HEAD)\" = 1; printf 'native-git-ok\\n'".to_string();
    if github {
        script.push_str("; gh api /user --silent; git ls-remote https://github.com/hraness/xcb.git HEAD >/dev/null; printf 'native-github-ok\\n'");
    }
    let arguments =
        json!({"argv":["/bin/sh","-c",script],"cwd":".","timeoutMs":60000,"network":"https"});
    let prompt = format!(
        "Run this native execution acceptance fixture. Call workspace_native_exec exactly once with exactly these arguments, without changing or splitting them: {}. Do not use any other tools, request other capabilities, inspect private paths, or repeat the command. Reply briefly after the tool result. The host validates the tool receipt rather than your reply.",
        serde_json::to_string(&arguments)?
    );
    let (mut config, _) = Config::load(store.root())?;
    let excluded_routes = BTreeSet::new();
    let excluded_accounts = BTreeSet::new();
    let decision = routing::smart_route(
        &store,
        &config,
        routing::RouteRequest {
            requirements: TaskRequirements::default(),
            task: &prompt,
            required_provider: Some(provider),
            preferred_provider: None,
            required_model: None,
            excluded_routes: &excluded_routes,
            excluded_accounts: &excluded_accounts,
            account: account.as_ref(),
        },
    )
    .await?;
    let session = store.create_session(
        &decision.account,
        decision.model.clone(),
        &workspace,
        now_ms(),
    )?;
    let message = Message {
        id: crate::new_id("native_fixture"),
        role: Role::User,
        text: prompt,
        at_ms: now_ms(),
        attachments: vec![],
        provenance: None,
    };
    let mut session = store.append_message(&session.id, session.revision, &message)?;
    let permit = native_backend::qualification_permit(&store, &session, scope.clone(), &arguments)?;
    session.requirements.native_execution = true;
    config.native_execution.scopes = vec![scope];
    config.capabilities = Default::default();
    config.auto_failover = false;
    config.turn_timeout_ms = 240000;
    let pin = process::Pin::load(store.root(), provider)?;
    let outcome = runner::run_with_pin(
        store.clone(),
        runner::RunInput {
            session: session.clone(),
            message,
            config,
            pane_generation: false,
        },
        cancel,
        Arc::new(|_| {}),
        Some(pin.clone()),
    )
    .await;
    let observed = store.messages(&session.id, 64)?.into_iter().any(|message| {
        if message.role != Role::Tool {
            return false;
        }
        let Some(body) = message.text.strip_prefix("workspace_native_exec: ") else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };
        let expected = if github {
            "native-shell-ok\nnative-dns-https-ok\nnative-git-ok\nnative-github-ok\n"
        } else {
            "native-shell-ok\nnative-dns-https-ok\nnative-git-ok\n"
        };
        value["stdout"] == expected
            && value["joined"] == true
            && value["exitCode"] == 0
            && value["sandbox"] == "native-workspace"
    });
    let joined = outcome.as_ref().is_ok_and(|outcome| outcome.facts.joined);
    let uncertain = !outcome
        .as_ref()
        .is_ok_and(|outcome| outcome.facts.effects != EffectState::Uncertain);
    if !joined || uncertain {
        let _ = fixture.keep();
    } else {
        std::fs::remove_file(permit)?;
    }
    let passed = observed
        && outcome.is_ok_and(|outcome| {
            outcome.state == State::Idle
                && outcome.facts.joined
                && !outcome.facts.pending_attention
                && outcome.facts.failure.is_none()
        });
    if !passed {
        return Err(Error::Guided {
            message: format!(
                "{provider} native checks did not pass; inspect session {}",
                session.id
            ),
            next: Some(format!(
                "xcb --json native inspect --session {}",
                session.id
            )),
        });
    }
    let current_pin = process::Pin::load(store.root(), provider)?;
    require_same_artifact(&pin, &current_pin)?;
    let receipt = ProviderQualification {
        version: 1,
        host_sha256: process::host_identity()?.1,
        provider_sha256: pin.sha256,
        provider,
        session: session.id,
        model: decision.model.key(),
        observed_at_ms: now_ms(),
        github_credentials: github,
        passed: true,
    };
    let path = store
        .root()
        .join(format!("qualification/native-{provider}.json"));
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    match private::read(&path, 16384) {
        Ok(previous) => private::replace(&path, &bytes, &crate::digest(previous))?,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(&path, &bytes)?
        }
        Err(error) => return Err(error),
    }
    Ok(receipt)
}

fn require_same_artifact(executed: &process::Pin, current: &process::Pin) -> Result<()> {
    if executed.provider != current.provider
        || executed.sha256 != current.sha256
        || executed.host_sha256 != current.host_sha256
    {
        return Err(Error::Unavailable(
            "provider changed during native verification; repeat qualification for the current build",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualification_rejects_provider_or_host_artifact_drift() {
        let executed = process::Pin {
            provider: Provider::Codex,
            executable: PathBuf::from("/synthetic/immutable-provider"),
            sha256: "a".repeat(64),
            version: "synthetic".into(),
            host_sha256: "b".repeat(64),
            observed_at_ms: 1,
        };
        require_same_artifact(&executed, &executed).unwrap();
        for changed in [
            process::Pin {
                sha256: "c".repeat(64),
                ..executed.clone()
            },
            process::Pin {
                host_sha256: "d".repeat(64),
                ..executed.clone()
            },
            process::Pin {
                provider: Provider::Claude,
                ..executed.clone()
            },
        ] {
            assert!(require_same_artifact(&executed, &changed).is_err());
        }
    }

    #[tokio::test]
    async fn explicit_verification_account_cannot_cross_provider_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        let account = store
            .add_account(Provider::Codex, "Fixture", 1, None)
            .unwrap();
        let (_cancel, cancelled) = watch::channel(false);
        let error = verify_for_account(
            store.clone(),
            Provider::Claude,
            Some(account.id),
            false,
            cancelled,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("another provider"));
        assert!(store.unsettled_runs().unwrap().is_empty());
    }
}
