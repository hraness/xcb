use crate::{
    Error, Result, claude,
    process::StreamProcess,
    protocol::{Event, Prompt, Protocol},
    runner,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
use xcb_core::models::ModelChoice;

pub(crate) struct ClaudeProtocol {
    tools: bool,
    cwd: PathBuf,
    model: ModelChoice,
    pending: BTreeMap<String, Value>,
    completed_output: u64,
    current_output: u64,
}
impl ClaudeProtocol {
    pub(crate) fn new(tools: bool, cwd: PathBuf, model: ModelChoice) -> Self {
        Self {
            tools,
            cwd,
            model,
            pending: BTreeMap::new(),
            completed_output: 0,
            current_output: 0,
        }
    }
}
impl Protocol for ClaudeProtocol {
    async fn initialize(
        &mut self,
        process: &mut StreamProcess,
        instructions: &str,
    ) -> Result<Vec<ModelChoice>> {
        runner::handshake(process, self.tools, instructions).await
    }
    async fn start(&mut self, process: &mut StreamProcess, prompt: Prompt) -> Result<()> {
        let mut content = vec![json!({"type":"text","text":prompt.text})];
        for image in prompt.images {
            content.push(json!({"type":"image","source":{"type":"base64","media_type":image.media_type,"data":image.base64}}));
        }
        process.send(&json!({"type":"user","session_id":"","parent_tool_use_id":null,"message":{"role":"user","content":content}})).await
    }
    async fn receive(&mut self, process: &mut StreamProcess, frame: &[u8]) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        if frame.len() > xcb_core::MAX_JSON_BYTES {
            return Err(Error::Protocol("frame limit"));
        }
        // One parse per frame: the streamed usage counter and the event
        // classification read the same value.
        let raw: Value = serde_json::from_slice(frame)?;
        match raw.pointer("/event/type").and_then(Value::as_str) {
            Some("message_start") => {
                self.completed_output = self
                    .completed_output
                    .checked_add(self.current_output)
                    .filter(|n| *n <= xcb_core::usage::COUNTER_LIMIT)
                    .ok_or(Error::Protocol("stream token counter"))?;
                self.current_output = 0;
            }
            Some("message_delta") => {
                if let Some(total) = raw.pointer("/event/usage/output_tokens") {
                    self.current_output = total
                        .as_u64()
                        .filter(|n| {
                            *n >= self.current_output && *n <= xcb_core::usage::COUNTER_LIMIT
                        })
                        .ok_or(Error::Protocol("stream token counter"))?;
                    let output = self
                        .completed_output
                        .checked_add(self.current_output)
                        .filter(|n| *n <= xcb_core::usage::COUNTER_LIMIT)
                        .ok_or(Error::Protocol("stream token counter"))?;
                    events.push(Event::OutputTokens(output));
                }
            }
            _ => (),
        }
        match claude::parse_value(raw)? {
            claude::Event::Initialize(value) => {
                runner::validate_init(&value, &self.cwd, &self.model, self.tools)?;
                events.push(Event::Ready);
            }
            claude::Event::Delta { thinking, text } => events.push(Event::Delta { thinking, text }),
            claude::Event::Assistant { text } => events.push(Event::Assistant(text)),
            claude::Event::Quota {
                window,
                utilization,
                resets_at_ms,
                failure,
                notice,
            } => {
                if let Some(notice) = notice {
                    events.push(Event::Diagnostic(runner::Diagnostic::notice(notice)));
                }
                events.push(Event::Quota {
                    window,
                    used_percent: utilization.map(|used| used * 100.0),
                    resets_at_ms,
                    failure,
                });
            }
            claude::Event::Result {
                terminal,
                text,
                models,
                failure,
            } => {
                // The classification precedes the result so the host settles
                // the account (NeedsAction, not Failed) from the same batch.
                if let Some(failure) = failure {
                    events.push(Event::Quota {
                        window: None,
                        used_percent: None,
                        resets_at_ms: None,
                        failure: Some(failure),
                    });
                }
                events.push(Event::Result {
                    terminal,
                    text,
                    models,
                });
            }
            claude::Event::Subagent {
                id,
                status,
                label,
                model,
            } => events.push(Event::Subagent {
                id,
                status,
                label,
                model,
            }),
            claude::Event::Control(envelope) => {
                let request = envelope
                    .get("request")
                    .ok_or(Error::Protocol("control request"))?;
                if request.get("subtype").and_then(Value::as_str) == Some("can_use_tool") {
                    runner::control(process, &envelope, json!({"behavior":"deny","message":"xcb will not manufacture permission; human attention is required"})).await?;
                    events.push(Event::Attention);
                } else if request.get("subtype").and_then(Value::as_str) == Some("mcp_message") {
                    if request.pointer("/message/method").and_then(Value::as_str)
                        == Some("tools/call")
                    {
                        // Validate server/version/request identity before any host
                        // tool can execute; validating only its later reply is too late.
                        runner::mcp_reply(request, self.tools, Some(Value::Null))?;
                        let id = envelope
                            .get("request_id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty() && id.len() <= 160)
                            .ok_or(Error::Protocol("tool call identity"))?
                            .to_owned();
                        let name = request
                            .pointer("/message/params/name")
                            .and_then(Value::as_str)
                            .ok_or(Error::Protocol("tool name"))?
                            .to_owned();
                        let arguments = request
                            .pointer("/message/params/arguments")
                            .ok_or(Error::Protocol("tool arguments"))?
                            .clone();
                        if self.pending.len() >= 128 || self.pending.contains_key(&id) {
                            return Err(Error::Protocol(
                                "duplicate or excessive pending tool call",
                            ));
                        }
                        self.pending.insert(id.clone(), envelope);
                        events.push(Event::Tool {
                            id,
                            name,
                            arguments,
                        });
                    } else {
                        let response = runner::mcp_reply(request, self.tools, None)?;
                        runner::control(process, &envelope, response).await?;
                    }
                } else {
                    return Err(Error::Protocol("unhandled provider control request"));
                }
            }
            claude::Event::Notice | claude::Event::ControlResponse(_) => (),
        }
        Ok(events)
    }
    async fn reply(&mut self, process: &mut StreamProcess, id: &str, result: Value) -> Result<()> {
        let envelope = self
            .pending
            .remove(id)
            .ok_or(Error::Protocol("unknown tool reply"))?;
        let request = envelope
            .get("request")
            .ok_or(Error::Protocol("control request"))?;
        let response = runner::mcp_reply(request, self.tools, Some(result))?;
        runner::control(process, &envelope, response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{Id, Provider, models::Mode};
    #[tokio::test]
    async fn foreign_or_malformed_mcp_envelopes_never_emit_executable_tools() {
        let root = tempfile::tempdir().unwrap();
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("fixture-model").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let mut protocol = ClaudeProtocol::new(true, root.path().to_owned(), model);
        let mut process = StreamProcess::spawn(tokio::process::Command::new("/bin/cat")).unwrap();
        for (server, version, id) in [
            ("foreign", "2.0", json!(1)),
            ("xcb", "1.0", json!(1)),
            ("xcb", "2.0", json!({})),
        ] {
            let frame = json!({"type":"control_request","request_id":"fixture","request":{"subtype":"mcp_message","server_name":server,"message":{"jsonrpc":version,"id":id,"method":"tools/call","params":{"name":"workspace_remove","arguments":{"path":"valuable.txt","expectedRevision":"a".repeat(64)}}}}});
            assert!(
                protocol
                    .receive(&mut process, &serde_json::to_vec(&frame).unwrap())
                    .await
                    .is_err()
            );
            assert!(protocol.pending.is_empty());
        }
        assert!(process.join().await);
    }
}
