use super::*;

#[cfg(target_os = "macos")]
mod metadata_fixture;
#[cfg(target_os = "macos")]
mod native_fixture;

fn protocol() -> DevinProtocol {
    let options = DevinOptions {
        cwd: PathBuf::from("/private/tmp/xcb-fixture"),
        model: ModelChoice {
            provider: Provider::Devin,
            id: Id::new("swe-1-6-fast").unwrap(),
            label: "Fixture".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        },
        tools: false,
        metadata_only: false,
    };
    let mut p = DevinProtocol::new(options, None).unwrap();
    p.options.tools = true;
    p.session = Some("fixture-session".into());
    p.ready = true;
    p.prompt_id = Some(7);
    p.mcp_initialized = true;
    p.listed = true;
    p
}
fn declaration(id: &str, name: &str, args: Value) -> Value {
    let qualified = format!("mcp__xcb__{name}");
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"tool_call","toolCallId":id,"rawInput":args,"_meta":{"cognition.ai/toolName":qualified,"cognition.ai/inferenceToolName":qualified,"cognition.ai/eventType":"mcp_tool_call"}}}})
}
fn permission(callback: &str, call: &str) -> Value {
    json!({"jsonrpc":"2.0","id":callback,"method":"session/request_permission","params":{"sessionId":"fixture-session","toolCall":{"toolCallId":call},"options":[{"optionId":"allow_once","kind":"allow_once"},{"optionId":"allow_session","kind":"allow_always"}]}})
}
fn mcp(id: u64, name: &str, args: Value) -> Request {
    let (reply, _) = oneshot::channel();
    Request {
        value: json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}),
        bytes: 100,
        reply,
    }
}

#[test]
fn only_an_exact_preceding_broker_declaration_receives_one_approval() {
    let mut p = protocol();
    let (_, reply) = p.accept(permission("unknown", "orphan")).unwrap();
    assert_eq!(reply[0]["result"]["outcome"]["outcome"], "cancelled");
    p.accept(declaration("call-1", "workspace_read", json!({"path":"a"})))
        .unwrap();
    let (_, reply) = p.accept(permission("permission-1", "call-1")).unwrap();
    assert_eq!(
        reply[0]["result"]["outcome"],
        json!({"outcome":"selected","optionId":"allow_once"})
    );
    let (_, reply) = p.accept(permission("permission-2", "call-1")).unwrap();
    assert_eq!(reply[0]["result"]["outcome"]["outcome"], "cancelled");
    assert!(p.accept(permission("permission-2", "call-1")).is_err());
}

#[test]
fn spoofed_metadata_changed_arguments_and_session_substitution_fail_closed() {
    for key in [
        "cognition.ai/toolName",
        "cognition.ai/inferenceToolName",
        "cognition.ai/eventType",
    ] {
        let mut value = declaration("call-1", "workspace_read", json!({"path":"a"}));
        value["params"]["update"]["_meta"][key] = json!("forged");
        assert!(protocol().accept(value).is_err());
    }
    let mut p = protocol();
    p.accept(declaration("call-1", "workspace_read", json!({"path":"a"})))
        .unwrap();
    let changed = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"pending","rawInput":{"path":"b"}}}});
    assert!(p.accept(changed).is_err());
    let mut foreign = permission("callback", "call-1");
    foreign["params"]["sessionId"] = json!("other");
    assert!(p.accept(foreign).is_err());
}

#[test]
fn bridge_requires_unique_permission_and_matching_arguments() {
    let mut p = protocol();
    assert!(
        p.mcp(mcp(1, "workspace_read", json!({"path":"a"})))
            .is_err()
    );
    p.accept(declaration("call-1", "workspace_read", json!({"path":"a"})))
        .unwrap();
    p.accept(permission("approve-1", "call-1")).unwrap();
    assert!(
        p.mcp(mcp(2, "workspace_read", json!({"path":"b"})))
            .is_err()
    );
    let events = p
        .mcp(mcp(3, "workspace_read", json!({"path":"a"})))
        .unwrap();
    assert!(
        matches!(&events[..],[Event::Tool{id,name,arguments}] if id=="call-1"&&name=="workspace_read"&&arguments==&json!({"path":"a"}))
    );
    assert!(
        p.mcp(mcp(4, "workspace_read", json!({"path":"a"})))
            .is_err()
    );
    assert!(
        p.accept(json!({"jsonrpc":"2.0","id":7,"result":{"stopReason":"end_turn"}}))
            .is_err()
    );
}

#[test]
fn identical_concurrent_calls_are_ambiguous_and_cannot_execute() {
    let mut p = protocol();
    for i in 1..=2 {
        let id = format!("call-{i}");
        p.accept(declaration(&id, "workspace_read", json!({"path":"a"})))
            .unwrap();
        p.accept(permission(&format!("approve-{i}"), &id)).unwrap();
    }
    assert!(
        p.mcp(mcp(1, "workspace_read", json!({"path":"a"})))
            .is_err()
    );
}

#[test]
fn completed_native_notebook_is_never_treated_as_brokered_work() {
    let mut p = protocol();
    let mut native = declaration(
        "native",
        "workspace_read",
        json!({"notebook_path":"fixture.ipynb"}),
    );
    native["params"]["update"]["_meta"] = json!({"cognition.ai/inferenceToolName":"notebook_read"});
    p.accept(native).unwrap();
    let (_, reply) = p.accept(permission("deny", "native")).unwrap();
    assert_eq!(reply[0]["result"]["outcome"]["outcome"], "cancelled");
    assert!(p.accept(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"native","status":"completed"}}})).is_err());
}

#[test]
fn usage_and_config_drift_are_rejected() {
    let mut p = protocol();
    assert!(p.accept(json!({"jsonrpc":"2.0","id":7,"result":{"stopReason":"end_turn","usage":{"inputTokens":5,"outputTokens":3,"totalTokens":9}}})).is_err());
    assert!(p.validate_options(&json!({"configOptions":[{"id":"model","currentValue":"other"},{"id":"mode","currentValue":"accept-edits"}]})).is_err());
}

#[test]
fn models_are_bounded_deduplicated_and_provider_scoped() {
    let value = json!({"configOptions":[{"id":"model","type":"select","options":[{"value":"swe-1-6-fast","name":"SWE"}]}]});
    let models = parse_models(&value, 1).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider, Provider::Devin);
    let mut duplicate = value;
    let row = duplicate["configOptions"][0]["options"][0].clone();
    duplicate["configOptions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .push(row);
    assert!(parse_models(&duplicate, 1).is_err());
}

#[test]
fn accepted_attachment_size_fits_acp_and_aggregate_overflow_is_actionable() {
    use crate::protocol::ImageInput;
    let image = || ImageInput {
        media_type: "image/png".into(),
        base64: "A".repeat(MAX_IMAGE_BASE64),
    };
    let p = protocol();
    let wire = p
        .prompt_wire(
            Prompt {
                text: "Inspect the image".into(),
                images: vec![image()],
            },
            8,
        )
        .unwrap();
    let bytes = serde_json::to_vec(&wire).unwrap();
    assert!(bytes.len() > MAX_JSON_BYTES && bytes.len() < MAX_WIRE_BYTES);
    assert!(protocol().envelope(&bytes).is_ok());
    let error = p
        .prompt_wire(
            Prompt {
                text: "Inspect both".into(),
                images: vec![image(), image()],
            },
            8,
        )
        .unwrap_err();
    assert!(matches!(error, Error::Unavailable(message) if message.contains("remove an image")));
    assert!(
        p.prompt_wire(
            Prompt {
                text: "x".repeat(MAX_PROMPT_BYTES + 1),
                images: vec![]
            },
            8
        )
        .is_err()
    );
}

#[test]
fn full_retained_context_and_separate_host_instructions_fit_the_prompt() {
    let mut p = protocol();
    p.instructions = "i".repeat(MAX_TEXT_BYTES);
    let wire = p
        .prompt_wire(
            Prompt {
                text: "c".repeat(MAX_PROMPT_BYTES),
                images: vec![],
            },
            8,
        )
        .unwrap();
    let text = wire["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(text.len() > MAX_PROMPT_BYTES + MAX_TEXT_BYTES);
    assert!(serde_json::to_vec(&wire).unwrap().len() < MAX_WIRE_BYTES);
}

#[test]
fn image_echo_is_bounded_input_and_never_assistant_output() {
    let value = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":{"sessionUpdate":"user_message_chunk","content":{"type":"image","mimeType":"image/png","data":"AAAA","detail":null}}}});
    let (events, replies) = protocol().accept(value.clone()).unwrap();
    assert!(events.is_empty() && replies.is_empty());
    let mut p = protocol();
    p.completed = true;
    assert!(p.accept(value).is_err());
}

#[test]
fn mcp_negotiates_only_supported_versions_and_retains_tool_authority() {
    for (proposed, selected) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-06-18"),
        ("2099-12-31", "2025-06-18"),
    ] {
        let mut p = protocol();
        p.mcp_initialized = false;
        p.listed = false;
        let (reply, mut receive) = oneshot::channel();
        let events = p.mcp(Request {
            value: json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":proposed,"capabilities":{"sampling":{},"roots":{"listChanged":true}},"clientInfo":{"name":"fixture","version":"1"}}}),
            bytes: 256,
            reply,
        }).unwrap();
        assert!(events.is_empty());
        let response = receive.try_recv().unwrap().unwrap();
        assert_eq!(response["result"]["protocolVersion"], selected);
        assert_eq!(response["result"]["capabilities"], json!({"tools":{}}));
        assert_eq!(p.mcp_proposed_version.as_deref(), Some(proposed));
        assert!(p.mcp_initialized);
        assert!(
            p.mcp(mcp(2, "workspace_read", json!({"path":"a"})))
                .is_err()
        );
    }
    for invalid in [
        "",
        "latest",
        "2025-11-25\n",
        "2025/11/25",
        "２０２５-１１-２５",
    ] {
        let mut p = protocol();
        p.mcp_initialized = false;
        let (reply, _receive) = oneshot::channel();
        assert!(p.mcp(Request {
            value: json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":invalid}}),
            bytes: 128,
            reply,
        }).is_err());
        assert!(!p.mcp_initialized);
        assert!(p.mcp_proposed_version.is_none());
    }
}

#[test]
fn mcp_progress_metadata_is_bounded_and_cannot_change_tool_authority() {
    for metadata in [
        json!({}),
        json!({"progressToken":"progress-1"}),
        json!({"progressToken":42}),
    ] {
        let arguments = json!({"path":"a"});
        let mut p = protocol();
        let mut unapproved = mcp(1, "workspace_read", arguments.clone());
        unapproved.value["params"]["_meta"] = metadata.clone();
        assert!(p.mcp(unapproved).is_err());
        p.accept(declaration("call-1", "workspace_read", arguments.clone()))
            .unwrap();
        p.accept(permission("approval-1", "call-1")).unwrap();
        let mut changed = mcp(2, "workspace_read", json!({"path":"b"}));
        changed.value["params"]["_meta"] = metadata.clone();
        assert!(p.mcp(changed).is_err());
        let mut approved = mcp(3, "workspace_read", arguments.clone());
        approved.value["params"]["_meta"] = metadata;
        let events = p.mcp(approved).unwrap();
        assert!(
            matches!(&events[..], [Event::Tool { id, name, arguments: observed }] if id == "call-1" && name == "workspace_read" && observed == &arguments)
        );
        assert!(p.mcp_metadata_seen);
    }
    for invalid in [
        json!(null),
        json!([]),
        json!({"progressToken":{}}),
        json!({"progressToken":true}),
        json!({"progressToken":1.5}),
        json!({"progressToken":"x".repeat(161)}),
        json!({"progressToken":"p","name":"workspace_write"}),
    ] {
        let mut p = protocol();
        p.accept(declaration("call-1", "workspace_read", json!({"path":"a"})))
            .unwrap();
        p.accept(permission("approval-1", "call-1")).unwrap();
        let mut request = mcp(1, "workspace_read", json!({"path":"a"}));
        request.value["params"]["_meta"] = invalid;
        assert!(p.mcp(request).is_err());
        assert!(p.pending.is_empty());
        assert!(!p.calls["call-1"].bridged);
        assert!(!p.mcp_metadata_seen);
    }
}

#[test]
fn initialization_failure_keeps_only_operation_code_and_fixed_category() {
    for (message, category) in [
        (
            "TLS certificate failed",
            "TLS certificate or transport failure",
        ),
        (
            "401 Unauthorized",
            "authentication rejected; reconnect this account",
        ),
        ("Operation not permitted", "local provider access denied"),
        (
            "error sending request",
            "provider request or network failure",
        ),
        ("unexpected failure", "provider rejected the operation"),
    ] {
        let value = json!({"id":2,"error":{"code":-32002,"message":format!("{message}: Bearer SYNTHETIC_SECRET user@example.invalid /private/account/path"),"data":{"token":"SYNTHETIC_DATA_SECRET"}}});
        let error = initialization_response(&value, 2, "session/new").unwrap_err();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.contains("session/new"));
            assert!(rendered.contains("-32002"));
            assert!(rendered.contains(category));
            assert!(!rendered.contains("SYNTHETIC"));
            assert!(!rendered.contains("example.invalid"));
            assert!(!rendered.contains("/private/"));
        }
        assert!(matches!(
            initialization_response(&value, 3, "session/new"),
            Err(Error::Protocol("Devin initialization response identity"))
        ));
    }
    assert_eq!(
        initialization_response(
            &json!({"id":1,"result":{"protocolVersion":1}}),
            1,
            "initialize"
        )
        .unwrap(),
        json!({"protocolVersion":1})
    );
    let malformed = initialization_response(
        &json!({"id":2,"error":{"code":"SYNTHETIC_SECRET","message":null}}),
        2,
        "session/new",
    )
    .unwrap_err()
    .to_string();
    assert!(malformed.contains("-32603"));
    assert!(!malformed.contains("SYNTHETIC"));
}

#[test]
fn model_choices_errors_retain_only_shape_and_count() {
    for (choices, shape, count) in [
        (json!("synthetic-secret-not-for-output"), "string", None),
        (
            json!({"synthetic-private-key":"synthetic-secret"}),
            "object",
            None,
        ),
        (
            json!(vec![json!({"value":"swe-1-6-fast","name":"SWE"}); 4097]),
            "array",
            Some(4097),
        ),
    ] {
        let result = json!({"configOptions":[{"id":"model","type":"select","options":choices}]});
        let error = parse_models(&result, 1).unwrap_err();
        assert!(
            matches!(&error, Error::DevinModelChoices { shape: actual, count: actual_count } if *actual == shape && *actual_count == count)
        );
        assert!(!format!("{error} {error:?}").contains("synthetic-"));
    }
    let missing = json!({"configOptions":[{"id":"model","type":"select"}]});
    assert!(matches!(
        parse_models(&missing, 1),
        Err(Error::DevinModelChoices {
            shape: "missing",
            count: None
        })
    ));
}

#[test]
fn full_model_catalog_preserves_order_with_store_sized_bound() {
    let rows = (0..4096)
        .map(|i| json!({"value":format!("fixture-model-{i}"),"name":format!("Fixture model {i}")}))
        .collect::<Vec<_>>();
    let mut result = json!({"configOptions":[{"id":"model","type":"select","options":rows}]});
    let models = parse_models(&result, 1).unwrap();
    assert_eq!(models.len(), 4096);
    assert_eq!(models[0].id.as_str(), "fixture-model-0");
    assert_eq!(models[4095].id.as_str(), "fixture-model-4095");
    result["configOptions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .push(json!({"value":"fixture-overflow","name":"Overflow"}));
    assert!(matches!(
        parse_models(&result, 1),
        Err(Error::DevinModelChoices {
            shape: "array",
            count: Some(4097)
        })
    ));
    result["configOptions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .pop();
    result["configOptions"][0]["options"][4095]["value"] = json!("fixture-model-0");
    assert!(matches!(
        parse_models(&result, 1),
        Err(Error::Protocol("Devin duplicate model"))
    ));
}

#[test]
fn prompt_failure_keeps_only_fixed_diagnostics_after_identity_check() {
    let value = json!({"jsonrpc":"2.0","id":7,"error":{"code":-32002,"message":"error sending request: Bearer SYNTHETIC_SECRET user@example.invalid /private/account/path","data":{"token":"SYNTHETIC_DATA_SECRET"}}});
    let error = protocol().accept(value.clone()).err().unwrap();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(rendered.contains("session/prompt"));
        assert!(rendered.contains("-32002"));
        assert!(rendered.contains("provider request or network failure"));
        assert!(!rendered.contains("SYNTHETIC"));
        assert!(!rendered.contains("example.invalid"));
        assert!(!rendered.contains("/private/"));
    }
    let mut wrong_id = value;
    wrong_id["id"] = json!(8);
    assert!(matches!(
        protocol().accept(wrong_id),
        Err(Error::Protocol("Devin prompt response identity"))
    ));
    assert!(
        protocol()
            .accept(json!({"jsonrpc":"2.0","id":7,"result":{"stopReason":"end_turn"}}))
            .is_ok()
    );
}

#[test]
fn resource_limit_failures_keep_only_fixed_diagnostics_and_exact_identity() {
    for (code, kind) in [
        (-32011, json!(null)),
        (-32011, json!("untrusted-other-kind")),
        (-32002, json!("resource_exhausted")),
    ] {
        let value = json!({"jsonrpc":"2.0","id":7,"error":{
            "code":code,"message":"TLS request Bearer SYNTHETIC_SECRET /private/account/path",
            "data":{"cognition.ai/errorKind":kind,"token":"SYNTHETIC_DATA_SECRET"}
        }});
        let error = protocol().accept(value.clone()).err().unwrap();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.contains("session/prompt"));
            assert!(rendered.contains(&code.to_string()));
            assert!(rendered.contains("provider quota or resource limit reached"));
            assert!(!rendered.contains("SYNTHETIC"));
            assert!(!rendered.contains("/private/"));
            assert!(!rendered.contains("untrusted"));
        }
        let mut wrong_id = value.clone();
        wrong_id["id"] = json!(8);
        assert!(matches!(
            protocol().accept(wrong_id),
            Err(Error::Protocol("Devin prompt response identity"))
        ));
        assert!(matches!(
            initialization_response(&value, 8, "session/new"),
            Err(Error::Protocol("Devin initialization response identity"))
        ));
    }
    // Similar, oversized, and non-string metadata cannot claim this category.
    for kind in [
        json!("resource_exhausted_extra"),
        json!("RESOURCE_EXHAUSTED"),
        json!("resource_exhausted".repeat(1000)),
        json!({"resource_exhausted":true}),
    ] {
        let value = json!({"id":2,"error":{"code":-32002,"message":"unknown","data":{"cognition.ai/errorKind":kind}}});
        assert!(matches!(
            initialization_response(&value, 2, "session/new"),
            Err(Error::DevinRpc {
                method: "session/new",
                code: -32002,
                category: "provider rejected the operation"
            })
        ));
    }
}
