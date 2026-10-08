use crate::{
    Error, Result, claude,
    process::StreamProcess,
    protocol::{Event, Prompt, Protocol},
    runner,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
use xcb_core::models::ModelChoice;

#[derive(Clone, Copy)]
pub(crate) enum MetadataQuery {
    Usage,
    McpStatus,
    ContextSummary,
}

impl MetadataQuery {
    fn request(self) -> Value {
        let (id, request) = match self {
            Self::Usage => (
                "xcb_usage",
                json!({"subtype":"get_usage","skip_behaviors":true}),
            ),
            Self::McpStatus => ("xcb_mcp_status", json!({"subtype":"mcp_status"})),
            Self::ContextSummary => (
                "xcb_context_summary",
                json!({"subtype":"get_context_usage","detail":"summary"}),
            ),
        };
        json!({"type":"control_request","request_id":id,"request":request})
    }

    fn response(self, frame: &[u8]) -> Result<Option<Value>> {
        match claude::parse_event(frame)? {
            claude::Event::Notice => Ok(None),
            claude::Event::ControlResponse(value) => {
                if value.pointer("/response/request_id") != Some(&self.request()["request_id"]) {
                    return Err(Error::Protocol("metadata query response identity"));
                }
                if value.pointer("/response/subtype").and_then(Value::as_str) != Some("success") {
                    return Err(Error::Protocol("metadata query unavailable"));
                }
                value
                    .pointer("/response/response")
                    .cloned()
                    .map(Some)
                    .ok_or(Error::Protocol("metadata query response"))
            }
            _ => Err(Error::Protocol("unexpected metadata query frame")),
        }
    }
}

pub(crate) async fn metadata_query(
    process: &mut StreamProcess,
    query: MetadataQuery,
) -> Result<Value> {
    process.send(&query.request()).await?;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        for _ in 0..128 {
            let frame = process
                .frame()
                .await?
                .ok_or(Error::Protocol("metadata query connection ended"))?;
            if let Some(value) = query.response(&frame)? {
                return Ok(value);
            }
        }
        Err(Error::Protocol("metadata query frame limit"))
    })
    .await
    .map_err(|_| Error::Unavailable("metadata query timed out"))?
}

pub(crate) struct ClaudeProtocol {
    tools: bool,
    cwd: PathBuf,
    model: ModelChoice,
    pending: BTreeMap<String, Value>,
    completed_output: u64,
    current_output: u64,
    permission_denied: bool,
    active: bool,
    interrupted: bool,
    metadata: Option<claude::Metadata>,
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
            permission_denied: false,
            active: false,
            interrupted: false,
            metadata: None,
        }
    }
}
impl Protocol for ClaudeProtocol {
    fn account_identity(&self) -> (Option<String>, Option<String>) {
        self.metadata.as_ref().map_or((None, None), |metadata| {
            (
                metadata
                    .account
                    .email
                    .as_deref()
                    .and_then(crate::store::observed_email),
                metadata
                    .account
                    .subscription_type
                    .as_ref()
                    .map(|plan| format!("Claude {plan}")),
            )
        })
    }

    fn interruption(&mut self) -> Option<Value> {
        if !self.active || self.interrupted {
            return None;
        }
        self.interrupted = true;
        Some(
            json!({"type":"control_request","request_id":"xcb_interrupt","request":{"subtype":"interrupt"}}),
        )
    }

    async fn initialize(
        &mut self,
        process: &mut StreamProcess,
        instructions: &str,
    ) -> Result<Vec<ModelChoice>> {
        let metadata = runner::handshake(process, self.tools, instructions).await?;
        let models = metadata.models.clone();
        self.metadata = Some(metadata);
        Ok(models)
    }
    async fn start(&mut self, process: &mut StreamProcess, prompt: Prompt) -> Result<()> {
        self.permission_denied = false;
        self.active = true;
        self.interrupted = false;
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
                observations,
                failure,
                notice,
            } => {
                if let Some(notice) = notice {
                    events.push(Event::Diagnostic(runner::Diagnostic::notice(notice)));
                }
                if observations.is_empty() {
                    events.push(Event::Quota {
                        window: None,
                        used_percent: None,
                        resets_at_ms: None,
                        failure,
                    });
                }
                for (index, observation) in observations.into_iter().enumerate() {
                    events.push(Event::Quota {
                        window: Some(observation.window),
                        used_percent: Some(observation.utilization * 100.0),
                        resets_at_ms: observation.resets_at_ms,
                        failure: if index == 0 { failure } else { None },
                    });
                }
            }
            claude::Event::Result {
                mut terminal,
                text,
                models,
                mut failure,
            } => {
                self.active = false;
                if failure == Some(xcb_core::policy::Failure::Policy) || self.permission_denied {
                    self.record_denial(&mut events);
                    terminal = xcb_core::policy::Terminal::Failed;
                    failure = Some(xcb_core::policy::Failure::Policy);
                }
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
            claude::Event::Subagent { .. } => {
                return Err(Error::Protocol("Claude delegated turn is unqualified"));
            }
            claude::Event::PermissionDenied => {
                if self.record_denial(&mut events) {
                    // Preserve the policy fact even when the provider exits
                    // before sending a result.
                    events.push(Event::Quota {
                        window: None,
                        used_percent: None,
                        resets_at_ms: None,
                        failure: Some(xcb_core::policy::Failure::Policy),
                    });
                }
            }
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

impl ClaudeProtocol {
    fn record_denial(&mut self, events: &mut Vec<Event>) -> bool {
        if self.permission_denied {
            return false;
        }
        events.push(Event::Diagnostic(runner::Diagnostic::notice(
            "Claude denied a tool action; automatic continuation and provider switching are paused",
        )));
        events.push(Event::Attention);
        self.permission_denied = true;
        true
    }
}

// Drives provider or command-runner fixtures, which Windows builds refuse.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use xcb_core::{Id, Provider, models::Mode};
    #[test]
    fn metadata_queries_are_summary_only_correlated_and_never_dispatch_tools() {
        assert_eq!(
            MetadataQuery::ContextSummary.request()["request"],
            json!({"subtype":"get_context_usage","detail":"summary"})
        );
        assert_eq!(
            MetadataQuery::Usage.request()["request"]["skip_behaviors"],
            true
        );
        for query in [
            MetadataQuery::Usage,
            MetadataQuery::McpStatus,
            MetadataQuery::ContextSummary,
        ] {
            let id = query.request()["request_id"].clone();
            let valid = json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}});
            assert_eq!(
                query
                    .response(&serde_json::to_vec(&valid).unwrap())
                    .unwrap(),
                Some(json!({}))
            );
            for bad in [
                json!({"type":"control_response","response":{"subtype":"success","request_id":"foreign","response":{}}}),
                json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":"SYNTHETIC_PRIVATE_DETAIL"}}),
                json!({"type":"control_request","request_id":"tool","request":{"subtype":"mcp_message","server_name":"xcb","message":{"method":"tools/call"}}}),
                json!({"type":"control_request","request_id":"permission","request":{"subtype":"can_use_tool"}}),
                json!({"type":"result","subtype":"success","is_error":false,"result":"SYNTHETIC_PRIVATE_DETAIL"}),
            ] {
                let error = query
                    .response(&serde_json::to_vec(&bad).unwrap())
                    .unwrap_err();
                assert!(!error.to_string().contains("SYNTHETIC_PRIVATE_DETAIL"));
            }
        }
    }

    #[tokio::test]
    async fn metadata_queries_reject_eof_and_notice_floods_with_bounded_cleanup() {
        for script in [
            "exit 0",
            "i=0; while [ $i -lt 128 ]; do printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"status\",\"status\":\"compacting\"}'; i=$((i+1)); done; cat >/dev/null",
        ] {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.arg("-c").arg(script);
            let mut process = StreamProcess::spawn(command).unwrap();
            assert!(
                metadata_query(&mut process, MetadataQuery::McpStatus)
                    .await
                    .is_err()
            );
            assert!(process.join().await);
        }
    }

    #[tokio::test]
    async fn cooperative_interrupt_is_one_shot_and_only_for_an_active_turn() {
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
        let mut protocol = ClaudeProtocol::new(false, root.path().to_owned(), model);
        let mut process = StreamProcess::spawn(tokio::process::Command::new("/bin/cat")).unwrap();
        assert!(protocol.interruption().is_none());
        protocol
            .start(
                &mut process,
                Prompt {
                    text: "fixture".into(),
                    images: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(
            protocol.interruption(),
            Some(
                json!({"type":"control_request","request_id":"xcb_interrupt","request":{"subtype":"interrupt"}})
            )
        );
        assert!(protocol.interruption().is_none());
        protocol
            .receive(
                &mut process,
                br#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#,
            )
            .await
            .unwrap();
        assert!(protocol.interruption().is_none());
        assert!(process.join().await);
    }

    #[tokio::test]
    async fn advisory_denial_is_sticky_and_final_list_cannot_clear_it() {
        let model = ModelChoice {
            provider: Provider::Claude,
            id: Id::new("fixture-model").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let root = tempfile::tempdir().unwrap();
        let mut protocol = ClaudeProtocol::new(true, root.path().to_owned(), model);
        let mut process = StreamProcess::spawn(tokio::process::Command::new("/bin/cat")).unwrap();
        let denial = serde_json::to_vec(&json!({"type":"system","subtype":"permission_denied",
            "tool_name":"workspace_write","tool_use_id":"denied-1",
            "message":"SYNTHETIC_PRIVATE_DETAIL","decision_reason_type":"classifier"}))
        .unwrap();
        let events = protocol.receive(&mut process, &denial).await.unwrap();
        assert!(
            matches!(&events[..], [Event::Diagnostic(detail), Event::Attention,
                Event::Quota {failure: Some(xcb_core::policy::Failure::Policy), ..}]
            if !detail.as_str().contains("SYNTHETIC_PRIVATE_DETAIL"))
        );
        assert!(
            protocol
                .receive(&mut process, &denial)
                .await
                .unwrap()
                .is_empty()
        );
        let final_frame = serde_json::to_vec(&json!({"type":"result","subtype":"success",
            "is_error":false,"result":"Continue the task.","permission_denials":[]}))
        .unwrap();
        let events = protocol.receive(&mut process, &final_frame).await.unwrap();
        assert!(matches!(
            &events[..],
            [
                Event::Quota {
                    failure: Some(xcb_core::policy::Failure::Policy),
                    ..
                },
                Event::Result {
                    terminal: xcb_core::policy::Terminal::Failed,
                    ..
                }
            ]
        ));
        assert!(process.join().await);
    }
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
