//! The closed xcb command-payload union — the plaintext body inside a
//! sealed command envelope. Every remote effect parses through `decode`
//! before the managed layer sees it; `encode` produces the bytes a
//! controller seals. Bounds ride the 8KiB command-plaintext ceiling, with
//! per-field caps so one field can't consume the whole frame.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

/// Single-field character bound — workspace names, task ids, daemon
/// names, attention ids.
const FIELD_CHARS: usize = 256;
/// Free-text field bound — prompts, steer text, daemon messages,
/// attention answers.
const TEXT_CHARS: usize = 6 * 1024;

/// The plaintext command body, mirrored on the wire as a flat JSON object
/// with a `kind` tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandBody {
    /// Enqueue a managed task in a named workspace on the target device.
    TaskDispatch { workspace: String, prompt: String },
    /// Post follow-up text to a running managed task.
    TaskSteer { task: String, text: String },
    /// Cancel a running managed task.
    TaskCancel { task: String },
    /// Answer an attention item on the remote machine.
    AttentionAnswer { attention: String, answer: String },
    /// Post text to a named ALGAL daemon's inbox on the target device.
    DaemonSend { daemon: String, text: String },
    /// Ask the target to publish a fresh fleet projection now.
    ProjectionRefresh,
}

/// The wire `kind` a body maps to — one-to-one with
/// `wire::COMMAND_KINDS`.
pub fn kind_of(body: &CommandBody) -> &'static str {
    match body {
        CommandBody::TaskDispatch { .. } => "task_dispatch",
        CommandBody::TaskSteer { .. } => "task_steer",
        CommandBody::TaskCancel { .. } => "task_cancel",
        CommandBody::AttentionAnswer { .. } => "attention_answer",
        CommandBody::DaemonSend { .. } => "daemon_send",
        CommandBody::ProjectionRefresh => "projection_refresh",
    }
}

/// The exact key set each kind permits on the wire, `kind` included.
/// Anything else in the object rejects the whole body — wire values parse
/// exactly, never loosely.
fn allowed_keys(kind: &str) -> Result<&'static [&'static str]> {
    Ok(match kind {
        "task_dispatch" => &["kind", "prompt", "workspace"],
        "task_steer" => &["kind", "task", "text"],
        "task_cancel" => &["kind", "task"],
        "attention_answer" => &["kind", "answer", "attention"],
        "daemon_send" => &["daemon", "kind", "text"],
        "projection_refresh" => &["kind"],
        _ => return Err(invalid("command kind")),
    })
}

fn check_field(value: &str, what: &'static str) -> Result<()> {
    if value.is_empty() || value.chars().count() > FIELD_CHARS {
        return Err(invalid(what));
    }
    Ok(())
}

fn check_text(value: &str, what: &'static str) -> Result<()> {
    if value.is_empty() || value.chars().count() > TEXT_CHARS {
        return Err(invalid(what));
    }
    Ok(())
}

/// Serialize a body to its plaintext bytes after bound checks.
pub fn encode(body: &CommandBody) -> Result<Vec<u8>> {
    match body {
        CommandBody::TaskDispatch { workspace, prompt } => {
            check_field(workspace, "workspace")?;
            check_text(prompt, "prompt")?;
        }
        CommandBody::TaskSteer { task, text } => {
            check_field(task, "task")?;
            check_text(text, "text")?;
        }
        CommandBody::TaskCancel { task } => check_field(task, "task")?,
        CommandBody::AttentionAnswer { attention, answer } => {
            check_field(attention, "attention")?;
            check_text(answer, "answer")?;
        }
        CommandBody::DaemonSend { daemon, text } => {
            check_field(daemon, "daemon")?;
            check_text(text, "text")?;
        }
        CommandBody::ProjectionRefresh => (),
    }
    serde_json::to_vec(body).map_err(Error::Json)
}

/// Parse a decrypted payload back into the closed union: exact key set,
/// declared kind, and the same bounds as `encode` — a forged or stale
/// envelope fails here, not in the managed layer.
pub fn decode(plaintext: &[u8]) -> Result<CommandBody> {
    if plaintext.len() > super::lane::MAX_COMMAND_PLAINTEXT {
        return Err(invalid("command plaintext bound"));
    }
    let value: Value = serde_json::from_slice(plaintext).map_err(Error::Json)?;
    let object = value.as_object().ok_or(invalid("command body"))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(invalid("command kind"))?;
    let allowed = allowed_keys(kind)?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid("command body key"));
        }
    }
    let body: CommandBody = serde_json::from_value(value).map_err(Error::Json)?;
    // Round-trip the bounds so a hand-built JSON object can't widen them.
    encode(&body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(kind: &str) -> CommandBody {
        match kind {
            "task_dispatch" => CommandBody::TaskDispatch {
                workspace: "laptop".into(),
                prompt: "fix the flaky test".into(),
            },
            "task_steer" => CommandBody::TaskSteer {
                task: "t_1".into(),
                text: "try the other branch".into(),
            },
            "task_cancel" => CommandBody::TaskCancel { task: "t_1".into() },
            "attention_answer" => CommandBody::AttentionAnswer {
                attention: "a_1".into(),
                answer: "yes".into(),
            },
            "daemon_send" => CommandBody::DaemonSend {
                daemon: "watcher".into(),
                text: "status?".into(),
            },
            "projection_refresh" => CommandBody::ProjectionRefresh,
            _ => unreachable!(),
        }
    }

    #[test]
    fn every_wire_kind_round_trips() {
        // The payload union and the wire kind union are the same set.
        for kind in super::super::wire::COMMAND_KINDS {
            let body = body_of(kind);
            assert_eq!(kind_of(&body), *kind);
            let bytes = encode(&body).unwrap();
            assert_eq!(decode(&bytes).unwrap(), body);
        }
    }

    #[test]
    fn rejects_unknown_keys_and_kinds() {
        assert!(decode(br#"{"kind":"task_cancel","task":"t","extra":1}"#).is_err());
        assert!(decode(br#"{"kind":"self_destruct"}"#).is_err());
        assert!(decode(br#"{"task":"t"}"#).is_err());
        assert!(decode(b"not json").is_err());
    }

    #[test]
    fn bounds_reject_oversized_fields() {
        assert!(
            encode(&CommandBody::TaskDispatch {
                workspace: "w".repeat(FIELD_CHARS + 1),
                prompt: "p".into(),
            })
            .is_err()
        );
        assert!(
            encode(&CommandBody::DaemonSend {
                daemon: "d".into(),
                text: "x".repeat(TEXT_CHARS + 1),
            })
            .is_err()
        );
        assert!(
            encode(&CommandBody::TaskCancel {
                task: String::new()
            })
            .is_err()
        );
    }
}
