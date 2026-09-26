//! Remote access commands: `xcb link`, `xcb fleet`, `xcb dispatch`,
//! `xcb send`, `xcb remote` — the controller and enrollment surface over
//! the Convex relay. Everything here runs against an enrolled device in
//! `~/.xcb/cloud/` custody; plaintext never leaves the local sealing
//! boundary and stdout carries only the requested data.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde_json::{Value, json};

use xcb_runtime::cloud::{
    client::RelayClient,
    commands::{self, CommandBody},
    controller::Controller,
    crypto::DeviceIdentity,
    custody, link as relay_link, wire,
};
use xcb_runtime::{Error, Result};

#[derive(Subcommand)]
pub enum RemoteCommand {
    /// Post an account-key wrap so a newly enrolled device can decrypt
    /// fleet content. Run this on an already-linked device.
    Admit {
        /// Device id of the enrolling device (shown by `xcb link`).
        device: String,
    },
    /// Retire a lost or retired device. In-flight commands settle or
    /// expire against the new auth epoch; the id never rebinds.
    Revoke {
        /// Device id from `xcb fleet`.
        device: String,
    },
    /// Queue guidance for a running managed task on a remote device —
    /// the remote form of `xcb steer`.
    Steer {
        /// Target daemon device id from `xcb fleet`.
        device: String,
        /// Nonclosed task id on the target.
        task: String,
        /// Guidance text within the task's existing authority and budget.
        text: String,
    },
    /// Cancel a managed task on a remote device.
    Cancel {
        /// Target daemon device id from `xcb fleet`.
        device: String,
        /// Task id on the target; terminal tasks report alreadyTerminal.
        task: String,
    },
    /// Answer an attention item on a remote device. The attention id is
    /// the task id shown by `xcb attention --remote`.
    Answer {
        /// Target daemon device id from `xcb fleet`.
        device: String,
        /// Attention (task) id on the target.
        task: String,
        /// The answer text.
        text: String,
    },
    /// Ask a device to publish a fresh fleet projection now rather than
    /// on its usual cadence.
    Refresh {
        /// Target daemon device id from `xcb fleet`.
        device: String,
    },
    /// Read a posted command's lifecycle state and, once terminal, its
    /// result. `--wait` polls until it settles.
    Status {
        /// Command public id returned by dispatch, send, or a remote verb.
        command: String,
        /// Poll until the command reaches a terminal state.
        #[arg(long)]
        wait: bool,
    },
    /// Withdraw a still-pending command before the target device claims
    /// it. Already-claimed commands are untouched.
    Abort {
        /// Command public id returned by dispatch, send, or a remote verb.
        command: String,
    },
    /// Acknowledge a terminal command so retention can collect it.
    Ack {
        /// Command public id returned by dispatch, send, or a remote verb.
        command: String,
    },
}

/// The relay the CLI reaches when nothing overrides it: the anonymous
/// local Convex backend. Production linkage comes from `xcb link
/// --relay` or `XCB_RELAY_URL`, persisted into custody at enrollment.
pub const DEFAULT_DEPLOYMENT_URL: &str = "http://127.0.0.1:3210";
const RELAY_URL_ENV: &str = "XCB_RELAY_URL";

/// How long `xcb link` waits for an enrolled device to admit this one
/// before giving up.
const WRAP_WAIT: Duration = Duration::from_secs(10 * 60);
const WRAP_POLL: Duration = Duration::from_secs(3);

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

fn not_linked() -> Error {
    Error::from(xcb_core::Error::Invalid(
        "this machine is not linked — run `xcb link`",
    ))
}

/// Resolve the relay URL: explicit flag, then environment, then the
/// stored link, then the local-backend default.
fn deployment_url(flag: Option<&str>, state_root: &Path) -> Result<String> {
    if let Some(url) = flag {
        return Ok(url.to_string());
    }
    if let Ok(url) = std::env::var(RELAY_URL_ENV)
        && !url.is_empty()
    {
        return Ok(url);
    }
    if let Some(link) = custody::load_link(state_root)? {
        return Ok(link.deployment_url);
    }
    Ok(DEFAULT_DEPLOYMENT_URL.to_string())
}

/// One line of interactive input with the prompt on stderr — stdout is
/// reserved for data. `None` when stdin is not a terminal.
fn prompt(label: &str) -> Result<Option<String>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Ok(None);
    }
    eprint!("{label} ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn email_ok(value: &str) -> bool {
    // Matches `isEmailAddress` in the relay: bounded, one `@`, no
    // whitespace or controls.
    let bytes = value.as_bytes();
    value.len() <= 254
        && bytes.iter().filter(|byte| **byte == b'@').count() == 1
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_graphic() || *byte == b'.')
        && !value.starts_with('@')
        && !value.ends_with('@')
}

/// Assemble a controller from custody. Fails with `not_linked` text when
/// the machine has never enrolled, or the device is still waiting on a
/// key wrap.
async fn open_controller(state_root: &Path) -> Result<Controller> {
    let device = custody::load_device(state_root)?.ok_or_else(not_linked)?;
    let Some(link) = custody::load_link(state_root)? else {
        return Err(not_linked());
    };
    let session = custody::load_session(state_root)?.ok_or_else(not_linked)?;
    let Some((account_key, key_version)) = custody::load_account_key(state_root)? else {
        return Err(Error::from(xcb_core::Error::Invalid(
            "device enrolled but not admitted — run `xcb remote admit <device>` on a linked device",
        )));
    };
    Controller::open(
        device,
        account_key,
        key_version,
        session,
        &link.deployment_url,
        state_root,
    )
    .await
}

/// Options for `xcb link` — enrolment inputs plus the output flag.
pub struct LinkOptions<'a> {
    pub code: Option<&'a str>,
    pub controller: bool,
    pub email: Option<&'a str>,
    pub invite: Option<&'a str>,
    pub json_out: bool,
    pub label: Option<&'a str>,
    pub relay: Option<&'a str>,
}

/// `xcb link` — enroll this machine. `code` (or a terminal prompt)
/// completes the OTP verify; `--controller` enrolls a dispatch-only
/// device. A linked device that lacks the account key waits for a peer
/// `xcb remote admit` before returning.
pub async fn link(state_root: &Path, options: LinkOptions<'_>) -> Result<i32> {
    let LinkOptions {
        code,
        controller,
        email,
        invite,
        json_out,
        label,
        relay,
    } = options;
    // Fully linked only when all three custody records exist: the device
    // identity persists before enrollment so an interrupted link resumes,
    // the link record marks enrollment complete, and the account key
    // marks admission (a wrapped device can still be waiting on `admit`).
    if let Some(device) = custody::load_device(state_root)?
        && custody::load_link(state_root)?.is_some()
        && custody::load_account_key(state_root)?.is_some()
    {
        return print_json_or(
            json_out,
            || {
                println!("This machine is already linked (device {}).", device.device);
            },
            json!({
                "version": 1,
                "device": device.device,
                "linked": true,
            }),
        );
    }

    let url = deployment_url(relay, state_root)?;
    let mut client = RelayClient::connect(&url).await?;

    // A session in custody resumes enrollment directly — `xcb link` can
    // fail between verify and enroll and retrying must not burn a fresh
    // OTP against the rate limiter. A due token refreshes in place; a
    // dead refresh token falls through to the OTP flow.
    let resumed = match custody::load_session(state_root)? {
        Some(mut session) => {
            let live = relay_link::refresh_if_due(&mut client, state_root, &mut session)
                .await
                .is_ok();
            if live {
                client.authenticate(&session.token).await;
                eprintln!("Resuming link with the existing session.");
            }
            live
        }
        None => false,
    };

    if !resumed {
        // Phase 1 — request the code (silent rejection is the contract).
        let email = match email {
            Some(email) => email.to_string(),
            None => prompt("Email:")?.ok_or_else(|| {
                Error::from(xcb_core::Error::Invalid("email required — pass --email"))
            })?,
        };
        if !email_ok(&email) {
            return Err(invalid("email"));
        }
        // `--code` verifies a code an earlier `xcb link` already emailed —
        // a fresh request would invalidate it.
        if code.is_none() {
            relay_link::request_code(&mut client, &email, invite).await?;
        }

        // Phase 2 — verify.
        let code = match code {
            Some(code) => code.to_string(),
            None => {
                eprintln!("A sign-in code was emailed to {email}.");
                prompt("Code:")?.ok_or_else(|| {
                    Error::from(xcb_core::Error::Invalid("code required — pass --code"))
                })?
            }
        };
        if code.len() != 8 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("code — the emailed code is 8 digits"));
        }
        let session = relay_link::verify_code(&mut client, &email, &code).await?;
        custody::store_session(state_root, &session)?;
        client.authenticate(&session.token).await;
    }

    // Phase 3 — enroll + settle key custody.
    let device_class = if controller {
        wire::CONTROLLER_CLASS
    } else {
        wire::EXECUTOR_CLASS
    };
    let label = label
        .map(str::to_string)
        .or_else(hostname)
        .unwrap_or_else(|| "xcb".to_string());
    let outcome =
        relay_link::finish_link(&mut client, state_root, &url, device_class, &label).await?;

    match outcome.key_outcome {
        relay_link::KeyOutcome::Minted | relay_link::KeyOutcome::Adopted { .. } => {
            let adopted = matches!(outcome.key_outcome, relay_link::KeyOutcome::Adopted { .. });
            print_json_or(
                json_out,
                || {
                    println!("Linked as {device_class} device {}.", outcome.public_id);
                    if adopted {
                        println!("Account key adopted from an enrolled peer.");
                    }
                },
                json!({
                    "version": 1,
                    "device": outcome.public_id,
                    "deviceClass": device_class,
                    "linked": true,
                }),
            )?;
            Ok(0)
        }
        relay_link::KeyOutcome::AwaitingWrap => {
            eprintln!(
                "Enrolled as {} device {} — waiting for an enrolled device to run `xcb remote admit {}`.",
                device_class, outcome.public_id, outcome.public_id
            );
            wait_for_wrap(&mut client, state_root, &outcome.device, json_out).await
        }
    }
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty() && name.chars().count() <= 64)
}

/// Poll `relayEnvelopes:myKeyEnvelopes` until a wrap for this device
/// lands, then store the account key. Bounded by WRAP_WAIT.
async fn wait_for_wrap(
    client: &mut RelayClient,
    state_root: &Path,
    device: &DeviceIdentity,
    json_out: bool,
) -> Result<i32> {
    let deadline = Instant::now() + WRAP_WAIT;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::from(xcb_core::Error::Limit(
                "admission wait expired — rerun `xcb link` after `xcb remote admit`",
            )));
        }
        if let Some(mut session) = custody::load_session(state_root)? {
            relay_link::refresh_if_due(client, state_root, &mut session).await?;
        }
        tokio::select! {
            _ = tokio::time::sleep(WRAP_POLL) => {}
            _ = tokio::signal::ctrl_c() => {
                return Err(Error::from(xcb_core::Error::Invalid(
                    "link interrupted — rerun `xcb link` to resume",
                )));
            }
        }
        if let Some((key, version)) = relay_link::collect_key_wrap(client, device).await? {
            custody::store_account_key(state_root, &key, version)?;
            return print_json_or(
                json_out,
                || {
                    println!("Admitted — account key received.");
                },
                json!({
                    "version": 1,
                    "device": device.device,
                    "linked": true,
                    "admitted": true,
                }),
            );
        }
    }
}

/// `xcb fleet` — every enrolled device plus its published projections.
pub async fn fleet(state_root: &Path, json_out: bool) -> Result<i32> {
    /// A projection older than this is stale: the daemon publishes on a
    /// 30s cadence when contents change, so several missed intervals mean
    /// the device stopped writing, not just that nothing changed.
    const PROJECTION_STALE_MS: u64 = 120_000;
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
    let mut controller = open_controller(state_root).await?;
    let devices = controller.fleet().await?;
    let projections = controller.projections().await.unwrap_or_default();
    if json_out {
        let now = now_ms();
        let projection_summary: Vec<Value> = projections
            .iter()
            .map(|row| {
                json!({
                    "device": row.device,
                    "scope": row.scope,
                    "revision": row.revision,
                    "updatedAt": row.updated_at,
                    "stale": now.saturating_sub(row.updated_at as u64) > PROJECTION_STALE_MS,
                })
            })
            .collect();
        return print_json_or(
            true,
            || {},
            json!({
                "version": 1,
                "devices": devices.iter().map(|d| json!({
                    "device": d.device,
                    "deviceClass": d.device_class,
                    "label": d.label,
                    "online": d.online,
                    "status": d.status,
                    "keyVersion": d.key_version,
                })).collect::<Vec<_>>(),
                "projections": projection_summary,
            }),
        );
    }
    if devices.is_empty() {
        println!("No devices enrolled.");
        return Ok(0);
    }
    let now = now_ms();
    for device in &devices {
        let presence = if device.online { "online" } else { "offline" };
        println!(
            "{} · {} · {} · {} · {}",
            device.device, device.label, device.device_class, presence, device.status
        );
        for row in projections.iter().filter(|row| row.device == device.device) {
            let age = now.saturating_sub(row.updated_at as u64) / 1000;
            let stale = if age * 1000 > PROJECTION_STALE_MS {
                " · STALE"
            } else {
                ""
            };
            println!(
                "  projection {} · rev {} · {}s old{}",
                row.scope, row.revision, age, stale
            );
        }
    }
    Ok(0)
}

/// `xcb dispatch <device> <workspace>` — enqueue a managed task on a
/// remote machine. `prompt` rides the command payload.
pub async fn dispatch(
    state_root: &Path,
    device: &str,
    workspace: &str,
    prompt_text: &str,
    json_out: bool,
) -> Result<i32> {
    post(
        state_root,
        device,
        &CommandBody::TaskDispatch {
            workspace: workspace.to_string(),
            prompt: prompt_text.to_string(),
        },
        json_out,
    )
    .await
}

/// `xcb send <device> <daemon> <text>` — post to a remote daemon inbox.
pub async fn send(
    state_root: &Path,
    device: &str,
    daemon: &str,
    text: &str,
    json_out: bool,
) -> Result<i32> {
    post(
        state_root,
        device,
        &CommandBody::DaemonSend {
            daemon: daemon.to_string(),
            text: text.to_string(),
        },
        json_out,
    )
    .await
}

/// `xcb attention --remote` — decrypted fleet projections whose bodies
/// carry attention items.
pub async fn attention_remote(state_root: &Path, json_out: bool) -> Result<i32> {
    let mut controller = open_controller(state_root).await?;
    let projections = controller.projections().await?;
    if json_out {
        let rows: Vec<Value> = projections
            .iter()
            .map(|row| {
                json!({
                    "device": row.device,
                    "scope": row.scope,
                    "revision": row.revision,
                    "updatedAt": row.updated_at,
                    "projection": serde_json::from_slice::<Value>(&row.plaintext)
                        .unwrap_or(Value::Null),
                })
            })
            .collect();
        return print_json_or(true, || {}, json!({ "version": 1, "projections": rows }));
    }
    if projections.is_empty() {
        println!("No fleet projections published yet.");
        return Ok(0);
    }
    for row in &projections {
        let body = serde_json::from_slice::<Value>(&row.plaintext).unwrap_or(Value::Null);
        println!(
            "{} · {} · rev {} · {}",
            row.device,
            row.scope,
            row.revision,
            xcb_core::display_text(&body.to_string(), 4096)
        );
    }
    Ok(0)
}

/// Encode a command body, seal it to the target device and enqueue it.
/// Shared by every remote verb — `dispatch`, `send` and the `remote`
/// family all ride the same closed union.
async fn post(state_root: &Path, device: &str, body: &CommandBody, json_out: bool) -> Result<i32> {
    let plaintext = commands::encode(body)?;
    let mut controller = open_controller(state_root).await?;
    let sent = controller
        .dispatch(device, commands::kind_of(body), &plaintext, None, None)
        .await?;
    print_json_or(
        json_out,
        || {
            println!(
                "Posted to {} as {} (idempotency {}).",
                device, sent.command.public_id, sent.idempotency_key
            );
            if sent.replayed {
                println!("Matched an in-flight command — no second effect.");
            }
        },
        json!({
            "version": 1,
            "command": sent.command.public_id,
            "idempotencyKey": sent.idempotency_key,
            "state": sent.command.state.as_str(),
            "replayed": sent.replayed,
        }),
    )
}

/// `xcb remote …` — linkage verbs plus the rest of the command union a
/// controller agent needs: steer, cancel, answer, refresh, and the
/// posted-command lifecycle reads.
pub async fn remote(state_root: &Path, command: &RemoteCommand, json_out: bool) -> Result<i32> {
    match command {
        RemoteCommand::Admit { device } => {
            let mut controller = open_controller(state_root).await?;
            controller.admit(device).await?;
            print_json_or(
                json_out,
                || {
                    println!("Admitted {device} — it can now collect the account key.");
                },
                json!({ "version": 1, "admitted": device }),
            )
        }
        RemoteCommand::Revoke { device } => {
            let mut controller = open_controller(state_root).await?;
            controller.revoke(device).await?;
            print_json_or(
                json_out,
                || {
                    println!("Revoked {device}.");
                },
                json!({ "version": 1, "revoked": device }),
            )
        }
        RemoteCommand::Steer { device, task, text } => {
            post(
                state_root,
                device,
                &CommandBody::TaskSteer {
                    task: task.clone(),
                    text: text.clone(),
                },
                json_out,
            )
            .await
        }
        RemoteCommand::Cancel { device, task } => {
            post(
                state_root,
                device,
                &CommandBody::TaskCancel { task: task.clone() },
                json_out,
            )
            .await
        }
        RemoteCommand::Answer { device, task, text } => {
            post(
                state_root,
                device,
                &CommandBody::AttentionAnswer {
                    attention: task.clone(),
                    answer: text.clone(),
                },
                json_out,
            )
            .await
        }
        RemoteCommand::Refresh { device } => {
            post(
                state_root,
                device,
                &CommandBody::ProjectionRefresh,
                json_out,
            )
            .await
        }
        RemoteCommand::Status { command, wait } => {
            status(state_root, command, *wait, json_out).await
        }
        RemoteCommand::Abort { command } => {
            let mut controller = open_controller(state_root).await?;
            controller.cancel(command).await?;
            print_json_or(
                json_out,
                || {
                    println!("Aborted {command} — it cannot be claimed now.");
                },
                json!({ "version": 1, "aborted": command }),
            )
        }
        RemoteCommand::Ack { command } => {
            let mut controller = open_controller(state_root).await?;
            controller.acknowledge(command).await?;
            print_json_or(
                json_out,
                || {
                    println!("Acknowledged {command}.");
                },
                json!({ "version": 1, "acknowledged": command }),
            )
        }
    }
}

/// `xcb remote status` — read one posted command. With `--wait`, poll
/// until it settles (bounded); the exit code mirrors the outcome so an
/// agent can branch on it: `applied` is 0, every other terminal state 1,
/// an unknown id or an exhausted `--wait` is 2, and a still-running
/// command without `--wait` is informational 0.
async fn status(state_root: &Path, public_id: &str, wait: bool, json_out: bool) -> Result<i32> {
    const POLL: Duration = Duration::from_secs(2);
    const WAIT_MAX: Duration = Duration::from_secs(10 * 60);
    let mut controller = open_controller(state_root).await?;
    let deadline = Instant::now() + WAIT_MAX;
    let mut waited_out = false;
    let row = loop {
        let row = match controller.command(public_id).await {
            Ok(row) => row,
            // `get` is subject-scoped: a foreign or mistyped id rejects the
            // same way — either way it is not this fleet's command.
            Err(Error::Protocol("relay unknown-command")) => {
                print_json_or(
                    json_out,
                    || {
                        println!("{public_id} · unknown command");
                    },
                    json!({ "version": 1, "command": public_id, "error": "unknown command" }),
                )?;
                return Ok(2);
            }
            Err(error) => return Err(error),
        };
        if !wait || row.state.is_terminal() {
            break row;
        }
        if Instant::now() >= deadline {
            waited_out = true;
            break row;
        }
        tokio::time::sleep(POLL).await;
    };
    let result = if row.state.is_terminal() && row.result.is_some() {
        controller.open_result(&row).ok()
    } else {
        None
    };
    let result_text = result
        .as_deref()
        .map(|bytes| xcb_core::display_text(&String::from_utf8_lossy(bytes), 4096));
    print_json_or(
        json_out,
        || {
            println!(
                "{} · {} · target {}{}",
                row.public_id,
                row.state.as_str(),
                row.target_device_id,
                row.result_code
                    .as_deref()
                    .map(|code| format!(" · {code}"))
                    .unwrap_or_default(),
            );
            if let Some(text) = &result_text {
                println!("{text}");
            }
        },
        json!({
            "version": 1,
            "command": row.public_id,
            "state": row.state.as_str(),
            "resultCode": row.result_code,
            "target": row.target_device_id,
            "result": result_text,
        }),
    )?;
    Ok(if waited_out {
        2
    } else if row.state.is_terminal() && row.state != wire::CommandState::Applied {
        1
    } else {
        0
    })
}

fn print_json_or(json_out: bool, text: impl FnOnce(), value: Value) -> Result<i32> {
    if json_out {
        crate::print_json(value)?;
    } else {
        text();
    }
    Ok(0)
}
