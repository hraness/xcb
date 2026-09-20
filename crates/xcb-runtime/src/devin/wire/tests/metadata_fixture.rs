//! Opt-in, read-only account diagnostics. No prompt or session command exists
//! here. Output is reduced to fixed flags and numeric fields before publication.
use crate::{
    Error, Result, devin, digest, now_ms, private,
    process::{
        CaptureOutcome, Pin, StreamProcess, capture_supervised, environment, executable_digest,
    },
    runner::LaunchArtifacts,
    sandbox,
    store::Store,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use xcb_core::{Id, Provider, policy::EffectState, session::State};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    state: PathBuf,
    account: Id,
    executable: PathBuf,
    output: PathBuf,
}

const NUMERIC_FIELDS: &[&str] = &[
    "daily_quota_remaining_percent",
    "weekly_quota_remaining_percent",
    "overage_balance_micros",
    "daily_quota_reset_at_unix",
    "weekly_quota_reset_at_unix",
    "acu_consumed",
    "acu_limit",
    "credit_multiplier",
    "cost_per_million_input_tokens",
    "cost_per_million_output_tokens",
    "available_prompt_credits",
    "available_flow_credits",
];
const FLAGS: &[&str] = &[
    "is_active",
    "has_access",
    "is_premium",
    "is_capacity_limited",
    "disabled",
];
const MODELS: &[&str] = &[
    "swe-2-high",
    "swe-1-6",
    "swe-1-6-fast",
    "swe-1-7-lightning",
    "swe-1-7-lightning-medium",
    "swe-1-7",
    "swe-1-7-medium",
    "swe-2-max",
    "swe-2-medium",
];

fn collect(value: &Value, depth: usize, rows: &mut Vec<Value>) {
    if depth > 12 || rows.len() >= 4096 {
        return;
    }
    match value {
        Value::Object(object) => {
            let mut row = serde_json::Map::new();
            for key in NUMERIC_FIELDS {
                if let Some(value) = object.get(*key).filter(|v| v.is_number()) {
                    row.insert((*key).into(), value.clone());
                }
            }
            for key in FLAGS {
                if let Some(value) = object.get(*key).filter(|v| v.is_boolean()) {
                    row.insert((*key).into(), value.clone());
                }
            }
            for key in ["id", "model_uid", "model", "value"] {
                if let Some(value) = object
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|s| MODELS.contains(s))
                {
                    row.insert("observed_model".into(), json!(value));
                }
            }
            if !row.is_empty() {
                rows.push(Value::Object(row));
            }
            for child in object.values() {
                collect(child, depth + 1, rows);
            }
        }
        Value::Array(array) => {
            for child in array.iter().take(4096) {
                collect(child, depth + 1, rows);
            }
        }
        _ => {}
    }
}
fn sanitized(bytes: &[u8]) -> Value {
    let text = std::str::from_utf8(bytes).unwrap_or("");
    let mut rows = Vec::new();
    let parsed = serde_json::from_slice::<Value>(bytes).ok();
    if let Some(value) = &parsed {
        collect(value, 0, &mut rows);
    }
    // Never copy free-form output, keys, URLs, paths, account identifiers, or
    // provider-defined reasons; these booleans only record fixed phrases.
    let lower = text.to_ascii_lowercase();
    let flags = [
        "logged in",
        "authenticated",
        "quota",
        "usage",
        "daily",
        "weekly",
        "reset",
        "resource_exhausted",
        "quota exhausted",
        "plan",
        "credits",
    ]
    .into_iter()
    .map(|word| (word.to_owned(), json!(lower.contains(word))))
    .collect::<serde_json::Map<_, _>>();
    json!({"json":parsed.is_some(),"bytes":bytes.len(),"lines":text.lines().count(),
        "fixed_phrase_present":flags,"numeric_and_known_model_rows":rows})
}

// Catalogs exceed secret-login capture's deliberately small 64 KiB bound.
// Reuse the provider group/pipe custodian, cap stdout at 2 MiB, and require
// complete JSON. This proves content and physical joins, not an exit code.
async fn model_output(
    command: Command,
    store: &Store,
    run: &crate::store::RunRecord,
    mut cancel: watch::Receiver<bool>,
) -> CaptureOutcome {
    if *cancel.borrow() {
        return CaptureOutcome::NeverStarted(Error::Unavailable("sign-in cancelled"));
    }
    let mut process = match StreamProcess::spawn(command) {
        Ok(process) => process,
        Err(error @ Error::LaunchNotStarted(_)) => return CaptureOutcome::NeverStarted(error),
        Err(_) => return CaptureOutcome::Unproven,
    };
    let recorded = store.mark_spawned(run, process.pid());
    let result = match recorded {
        Err(error) => Err(error),
        Ok(_) => {
            let read = async {
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                (&mut process.stdout)
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                if bytes.len() > 2 * 1024 * 1024 {
                    return Err(Error::Protocol("login output limit"));
                }
                let parsed: Value = serde_json::from_slice(&bytes)?;
                if !parsed.is_object() && !parsed.is_array() {
                    return Err(Error::Protocol("metadata JSON container"));
                }
                Ok(bytes)
            };
            tokio::select! {
                biased;
                _ = async { if !*cancel.borrow() { let _ = cancel.changed().await; } } =>
                    Err(Error::Unavailable("sign-in cancelled")),
                output = tokio::time::timeout(Duration::from_secs(45), read) =>
                    output.unwrap_or(Err(Error::Unavailable("sign-in timed out"))),
            }
        }
    };
    if process.join().await {
        CaptureOutcome::Joined(result)
    } else {
        CaptureOutcome::Unproven
    }
}

async fn command(
    store: &Store,
    spec: &Spec,
    args: &[&str],
    cancel: watch::Receiver<bool>,
) -> Result<Value> {
    // The host binds its own executable in memory. No persistent provider pin
    // changes: this diagnostic never re-pins the user's installed xcb.
    let pin = Pin {
        provider: Provider::Devin,
        executable: spec.executable.canonicalize()?,
        sha256: devin::BINARY_SHA256.into(),
        version: devin::VERSION.into(),
        host_sha256: executable_digest(&std::env::current_exe()?.canonicalize()?)?,
        observed_at_ms: now_ms(),
    };
    pin.verify()?;
    devin::runtime_admitted(&pin)?;
    let run = store.prepare_probe(&spec.account, None, now_ms())?;
    let prepared = (|| {
        let artifacts = LaunchArtifacts::create(store.root())?;
        let executable = pin.snapshot(artifacts.path())?;
        let scratch = private::directory(&artifacts.path().join("scratch"))?;
        let home = private::directory(&scratch.join("home"))?;
        let cwd = private::directory(&scratch.join("work"))?;
        private::directory(&home.join("tmp"))?;
        let config = private::directory(&home.join(".config/devin"))?;
        private::create(
            &config.join("config.json"),
            &serde_json::to_vec(&devin::configuration())?,
        )?;
        private::create(&config.join("mcp_config.json"), b"{\"mcpServers\":{}}")?;
        // No MCP helper is launched. Repeating the same exact provider literal
        // supplies the helper slot without admitting another executable.
        let policy =
            sandbox::devin_seatbelt(&executable, &executable, &scratch, &home, &config, None)?;
        let policy_hash = digest(policy.as_bytes());
        let policy_path = artifacts.path().join("sandbox.sb");
        private::create(&policy_path, policy.as_bytes())?;
        let token = devin::auth::token(store, &spec.account)?;
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-f")
            .arg(policy_path)
            .arg(executable)
            .arg("--config")
            .arg(config.join("config.json"))
            .args(args)
            .env_clear()
            .envs(environment(&home))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("WINDSURF_API_KEY", token.as_str())
            .current_dir(cwd);
        Ok::<_, Error>((artifacts, command, policy_hash))
    })();
    let (mut artifacts, command, policy_hash) = match prepared {
        Ok(value) => value,
        Err(error) => {
            store.settle(&run, State::Failed, now_ms())?;
            return Err(error);
        }
    };
    artifacts.retain_before_launch();
    let models = args == ["models", "list", "--format", "json"];
    let outcome = if models {
        model_output(command, store, &run, cancel).await
    } else {
        capture_supervised(command, 64 * 1024, Duration::from_secs(45), cancel, |pid| {
            store.mark_spawned(&run, pid).map(|_| ())
        })
        .await
    };
    let output = match outcome {
        CaptureOutcome::Unproven => return Err(Error::CleanupUnproven),
        CaptureOutcome::NeverStarted(error) => Err(error),
        CaptureOutcome::Joined(output) => output,
    };
    // Joined errors are safe to settle; unproven cleanup returned above and
    // keeps both the durable lease and launch directory in custody.
    store.settle(
        &run,
        if output.is_ok() {
            State::Idle
        } else {
            State::Failed
        },
        now_ms(),
    )?;
    artifacts.release_after_join(true, EffectState::None);
    Ok(match output {
        Ok(bytes) => {
            json!({"command":args,"joined":true,"succeeded":true,"exit_status_checked":!models,"policy_sha256":policy_hash,"output":sanitized(&bytes)})
        }
        Err(error) => {
            let category = match error {
                Error::Protocol("login output limit") => "output exceeded capture bound",
                Error::Unavailable("sign-in timed out") => "command deadline",
                Error::Unavailable("sign-in cancelled") => "command cancelled",
                Error::Unavailable("sign-in did not complete") => "command returned failure",
                Error::LaunchNotStarted(_) => "command did not start",
                _ => "host capture rejected output",
            };
            json!({"command":args,"joined":true,"succeeded":false,"policy_sha256":policy_hash,"failure_category":category})
        }
    })
}

#[tokio::test]
#[ignore = "explicit authorized live account metadata; no model inference or session commands"]
async fn read_only_account_metadata_under_production_profile() {
    let path =
        std::env::var_os("XCB_DEVIN_METADATA_SPEC").expect("explicit metadata spec required");
    let spec: Spec =
        serde_json::from_slice(&private::read(&PathBuf::from(path), 8192).unwrap()).unwrap();
    let (cancel, receiver) = watch::channel(false);
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let signal_task = tokio::spawn(async move {
        tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
        let _ = cancel.send(true);
    });
    let store = Store::open(&spec.state).unwrap();
    assert_eq!(
        store.account(&spec.account).unwrap().provider,
        Provider::Devin
    );
    let mut rows = Vec::new();
    for args in [
        &["auth", "status"][..],
        &["models", "list", "--format", "json"][..],
    ] {
        match command(&store, &spec, args, receiver.clone()).await {
            Ok(row) => rows.push(row),
            Err(_) => {
                rows.push(json!({"command":args,"custody_or_preparation_failed":true}));
                break;
            }
        }
        if *receiver.borrow() {
            break;
        }
    }
    signal_task.abort();
    let _ = signal_task.await;
    let receipt = json!({"schema":"xcb.devin-read-only-metadata.v1","observed_at_ms":now_ms(),
        "provider_sha256":devin::BINARY_SHA256,"provider_version":devin::VERSION,
        "metadata_only":true,"inference_attempts":0,"commands":rows});
    private::create(&spec.output, &serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!(
        "{}",
        json!({"metadata_commands":rows.len(),"all_joined":rows.iter().all(|row| row["joined"] == true),"sanitized_receipt_written":true})
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["joined"] == true));
}

#[test]
fn metadata_sanitizer_retains_only_known_numeric_fields() {
    let output = sanitized(br#"{"daily_quota_remaining_percent":0,"daily_quota_reset_at_unix":1789952400,"token":"SYNTHETIC_SECRET","private_key":{"model_uid":"swe-1-6-fast","credit_multiplier":1,"disabled":false,"reason":"SYNTHETIC_REASON"},"unknown":"/private/account/path"}"#);
    let rendered = output.to_string();
    assert!(rendered.contains("1789952400"));
    assert!(rendered.contains("swe-1-6-fast"));
    assert!(!rendered.contains("SYNTHETIC"));
    assert!(!rendered.contains("private_key"));
    assert!(!rendered.contains("/private/"));
}
