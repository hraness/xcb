use super::*;

fn codec() -> CodexProtocol {
    let model = ModelChoice {
        provider: Provider::Codex,
        id: Id::new("gpt-6-astra").unwrap(),
        label: "Astra".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: Some(Id::new("ultra").unwrap()),
        observed_at_ms: 1,
    };
    CodexProtocol::new(CodexOptions {
        cwd: "/synthetic/work".into(),
        account_home: "/synthetic/profile".into(),
        catalog_path: "/synthetic/catalog.json".into(),
        model,
        tools: true,
        metadata_only: false,
        admission: Admission {
            catalog_sha256: "a".repeat(64),
            models: ["gpt-6-astra".into()].into(),
        },
    })
    .unwrap()
}
fn started() -> CodexProtocol {
    let mut codec = codec();
    codec.initialized = true;
    codec.thread_id = Some("thread1".into());
    codec.turn_rpc = Some(7);
    let (events, _) = codec
        .accept(json!({"id":7,"result":{"turn":{"id":"turn1"}}}))
        .unwrap();
    assert!(matches!(events[0], Event::Ready { .. }));
    codec
}
fn notice(method: &str, item: Value) -> Value {
    json!({"method":method,"params":{"threadId":"thread1","turnId":"turn1","item":item}})
}
fn call_item() -> Value {
    json!({"type":"dynamicToolCall","id":"call1","tool":"workspace_read","namespace":null,"arguments":{"path":"note.txt"},"status":"inProgress"})
}
fn callback() -> Value {
    json!({"id":80,"method":"item/tool/call","params":{"threadId":"thread1","turnId":"turn1","callId":"call1","tool":"workspace_read","namespace":null,"arguments":{"path":"note.txt"}}})
}

#[test]
fn dynamic_tool_round_trip_is_bound_and_completed_once() {
    let mut c = started();
    c.accept(notice("item/started", call_item())).unwrap();
    let (events, _) = c.accept(callback()).unwrap();
    assert!(
        matches!(&events[0], Event::Tool { id, name, arguments } if id == "call1" && name == "workspace_read" && arguments["path"] == "note.txt")
    );
    let (wire, reply) = c
        .tool_response("call1", json!({"text":"contents","revision":"r"}))
        .unwrap();
    assert_eq!(wire["id"], 80);
    assert_eq!(reply["success"], true);
    c.calls.get_mut("call1").unwrap().response = Some(reply.clone());
    assert!(c.tool_response("call1", json!({})).is_err());
    let mut done = call_item();
    done["status"] = json!("completed");
    done["success"] = reply["success"].clone();
    done["contentItems"] = reply["contentItems"].clone();
    c.accept(notice("item/completed", done.clone())).unwrap();
    assert!(c.calls["call1"].completed);
    assert!(c.accept(notice("item/completed", done)).is_err());
}

#[test]
fn forged_or_replayed_tools_never_reach_the_broker() {
    assert!(started().accept(callback()).is_err());
    for field in ["threadId", "turnId", "tool", "callId", "namespace"] {
        let mut c = started();
        c.accept(notice("item/started", call_item())).unwrap();
        let mut request = callback();
        request["params"][field] = json!("wrong");
        assert!(c.accept(request).is_err(), "{field}");
    }
    let mut c = started();
    c.accept(notice("item/started", call_item())).unwrap();
    let mut request = callback();
    request["params"]["arguments"]["path"] = json!("different.txt");
    assert!(c.accept(request).is_err());
    let mut c = started();
    c.accept(notice("item/started", call_item())).unwrap();
    c.accept(callback()).unwrap();
    assert!(c.accept(callback()).is_err());
    let mut c = started();
    let mut item = call_item();
    item["tool"] = json!("exec_command");
    assert!(c.accept(notice("item/started", item)).is_err());
}

#[test]
fn native_execution_and_permission_requests_are_not_granted() {
    let mut c = started();
    let (_, replies) = c.accept(json!({"id":1,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread1","turnId":"turn1"}})).unwrap();
    assert_eq!(replies[0]["error"]["code"], -32601);
    assert!(
        c.accept(notice(
            "item/started",
            json!({"id":"native1","type":"commandExecution","command":"true"})
        ))
        .is_err()
    );
    assert!(started().accept(json!({"id":1,"method":"account/chatgptAuthTokens/refresh","params":{"threadId":"thread1","turnId":"turn1"}})).is_err());
}

#[test]
fn readiness_requires_the_matching_rpc_and_rejects_early_execution() {
    let mut c = codec();
    c.thread_id = Some("thread1".into());
    c.turn_rpc = Some(7);
    assert!(c.accept(notice("item/started", call_item())).is_err());
    c.accept(
        json!({"method":"turn/started","params":{"threadId":"thread1","turn":{"id":"early"}}}),
    )
    .unwrap();
    assert!(
        c.accept(json!({"id":7,"result":{"turn":{"id":"different"}}}))
            .is_err()
    );
    assert!(
        started()
            .accept(json!({"id":7,"result":{"turn":{"id":"turn1"}}}))
            .is_err()
    );
}

#[test]
fn usage_counts_caches_once_and_refuses_regression() {
    let value = json!({"totalTokens":150,"inputTokens":120,"cachedInputTokens":80,"cacheWriteInputTokens":10,"outputTokens":30,"reasoningOutputTokens":20});
    let (counters, total) = usage(&value).unwrap();
    assert_eq!(
        (
            counters.input,
            counters.cache_read,
            counters.cache_write,
            counters.output
        ),
        (30, 80, 10, 30)
    );
    assert_eq!(counters.total().unwrap(), total);
    let mut c = started();
    let notice = json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thread1","turnId":"turn1","tokenUsage":{"total":value,"last":value}}});
    assert!(matches!(
        c.accept(notice.clone()).unwrap().0[0],
        Event::OutputTokens(30)
    ));
    let mut changed = notice;
    changed["params"]["tokenUsage"]["total"]["outputTokens"] = json!(29);
    changed["params"]["tokenUsage"]["total"]["totalTokens"] = json!(149);
    assert!(c.accept(changed).is_err());
}

#[test]
fn sparse_quota_does_not_invent_a_reset_and_complete_quota_is_reported() {
    let mut c = started();
    let p = json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":50,"resetsAt":null}}}});
    assert!(c.accept(p.clone()).unwrap().0.is_empty());
    let mut p = p;
    p["params"]["rateLimits"]["primary"]["resetsAt"] = json!(2000000000);
    assert!(matches!(
        c.accept(p).unwrap().0[0],
        Event::Quota {
            used_percent: Some(50.0),
            resets_at_ms: Some(2000000000000),
            ..
        }
    ));
}

#[test]
fn terminal_requires_completed_tools_and_retains_final_text() {
    let terminal = json!({"method":"turn/completed","params":{"threadId":"thread1","turn":{"id":"turn1","status":"completed","error":null}}});
    let mut c = started();
    c.accept(notice("item/started", call_item())).unwrap();
    assert!(c.accept(terminal.clone()).is_err());
    let mut c = started();
    let message =
        json!({"id":"answer1","type":"agentMessage","text":"done","phase":"final_answer"});
    c.accept(notice("item/started", message.clone())).unwrap();
    c.accept(notice("item/completed", message)).unwrap();
    assert!(
        matches!(&c.accept(terminal.clone()).unwrap().0[0], Event::Result { terminal: Terminal::Completed, text, .. } if text == "done")
    );
    assert!(c.accept(terminal).is_err());
}

#[test]
fn fresh_catalog_preserves_all_qualified_efforts_and_rejects_duplicates() {
    let model = json!({"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"Astra","hidden":false,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"ultra"}]});
    let choices = parse_models(&json!({"data":[model],"nextCursor":null}), 42).unwrap();
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[1].effort.as_ref().unwrap().as_str(), "ultra");
    assert_eq!(choices[0].observed_at_ms, 42);
    assert!(parse_models(&json!({"data":[model,model],"nextCursor":null}), 42).is_err());
}

#[test]
fn unknown_executable_frames_and_wrong_envelopes_fail_closed() {
    let mut c = codec();
    for value in [
        json!({"id":1,"result":{},"error":{}}),
        json!({"id":1,"result":{},"method":"anything"}),
        json!({"id":1,"result":{},"extra":true}),
        json!({"method":"x","emittedAtMs":-1}),
    ] {
        assert!(c.envelope(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(
        started()
            .accept(
                json!({"method":"model/rerouted","params":{"threadId":"thread1","turnId":"turn1"}})
            )
            .is_err()
    );
}

#[test]
fn current_settings_shape_preserves_the_strict_controls() {
    let mut c = codec();
    c.thread_id = Some("thread1".into());
    c.turn_rpc = Some(7);
    let p = json!({"threadId":"thread1","threadSettings":{"model":"gpt-6-astra","modelProvider":"openai","effort":"ultra","approvalPolicy":"never","approvalsReviewer":"user","cwd":"/synthetic/work","sandboxPolicy":{"type":"readOnly","networkAccess":false},"multiAgentMode":"explicitRequestOnly","collaborationMode":{"mode":"default"},"serviceTier":null,"activePermissionProfile":null}});
    c.settings_update(&p).unwrap();
    let mut altered = p;
    altered["threadSettings"]["sandboxPolicy"]["networkAccess"] = json!(true);
    assert!(c.settings_update(&altered).is_err());
}

#[test]
fn native_configuration_readback_detects_authority_changes() {
    let native: Value = serde_json::from_str(include_str!("config-readback.json")).unwrap();
    let catalog = std::path::Path::new("/synthetic/catalog.json");
    config::validate_config(&native, catalog).unwrap();
    for pointer in [
        "/config/features/shell_tool",
        "/config/apps/_default/enabled",
        "/config/analytics/enabled",
    ] {
        let mut changed = native.clone();
        *changed.pointer_mut(pointer).unwrap() = json!(true);
        assert!(
            config::validate_config(&changed, catalog).is_err(),
            "{pointer}"
        );
    }
    for (key, value) in [
        ("model_provider", json!("custom")),
        ("model_catalog_json", json!("/different/catalog.json")),
        ("developer_instructions", json!("inherited")),
        ("mcp_servers", json!({"inherited":{}})),
    ] {
        let mut changed = native.clone();
        changed["config"][key] = value;
        assert!(config::validate_config(&changed, catalog).is_err(), "{key}");
    }
}

#[test]
fn exact_native_echo_trace_replays_with_current_wire_shapes() {
    // Recorded from the qualified binary; IDs and the synthetic echo tool are
    // mapped to stable local names. No account data or model prompts retained.
    let frames: Vec<Value> = serde_json::from_str(include_str!("wire-echo-frames.json")).unwrap();
    let mut c = codec();
    c.initialized = true;
    c.thread_id = Some("thread1".into());
    c.turn_rpc = Some(7);
    c.prompt =
        Some(json!([{"type":"text","text":"Synthetic no-auth protocol test.","text_elements":[]}]));
    let mut ready = 0;
    let mut tools = 0;
    let mut terminal = 0;
    for frame in frames {
        let frame = c.envelope(&serde_json::to_vec(&frame).unwrap()).unwrap();
        for event in c.accept(frame).unwrap().0 {
            match event {
                Event::Ready { .. } => ready += 1,
                Event::Tool { id, .. } => {
                    tools += 1;
                    let (_, response) = c
                        .tool_response(&id, json!({"text":"broker-echo-ok"}))
                        .unwrap();
                    c.calls.get_mut(&id).unwrap().response = Some(response);
                }
                Event::Result {
                    terminal: Terminal::Completed,
                    ref text,
                    ..
                } => {
                    assert_eq!(text, "synthetic-complete");
                    terminal += 1;
                }
                _ => (),
            }
        }
    }
    assert_eq!((ready, tools, terminal), (1, 1, 1));
}

#[test]
fn quota_read_prefers_multi_bucket_data_without_inventing_recovery() {
    let pool = Id::new("account1").unwrap();
    let value = json!({"rateLimits":{"primary":{"usedPercent":99,"resetsAt":2000}},"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":20,"resetsAt":2000},"secondary":{"usedPercent":1,"resetsAt":null}},"codex_extra":{"secondary":{"usedPercent":45,"resetsAt":3000}}},"ordinaryUsageAllowed":true});
    let points = parse_quotas(&value, &pool, 1000).unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[0].used_percent, 20.0);
    assert_eq!(points[0].window.as_str(), "codex.primary");
    let mut denied = value;
    denied["ordinaryUsageAllowed"] = json!(false);
    assert!(parse_quotas(&denied, &pool, 1000).is_err());
    assert!(
        parse_quotas(
            &json!({"rateLimits":{"primary":{"usedPercent":20,"resetsAt":1}}}),
            &pool,
            2000
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn selected_task_catalog_never_replaces_the_account_catalog() {
    assert!(!codec().refreshes_catalog());
    let mut probe = codec();
    probe.options.metadata_only = true;
    assert!(probe.refreshes_catalog());
}

#[test]
fn full_shared_prompt_and_attachment_fit_but_combined_overflow_is_rejected() {
    let mut c = codec();
    c.thread_id = Some("thread1".into());
    let image = || crate::protocol::ImageInput {
        media_type: "image/png".into(),
        base64: "A".repeat(IMAGE_BASE64_BYTES),
    };
    let request = c
        .turn_request(Prompt {
            text: "p".repeat(PROMPT_BYTES),
            images: vec![image()],
        })
        .unwrap();
    assert!(serde_json::to_vec(&request).unwrap().len() < WIRE_FRAME_BYTES);
    let mut c = codec();
    c.thread_id = Some("thread1".into());
    assert!(
        c.turn_request(Prompt {
            text: String::new(),
            images: vec![image(), image()]
        })
        .is_err()
    );
    assert!(c.turn_rpc.is_none());
    let mut c = codec();
    assert!(
        c.turn_request(Prompt {
            text: "p".repeat(PROMPT_BYTES + 1),
            images: vec![]
        })
        .is_err()
    );
}

#[test]
fn native_input_echo_can_exceed_one_mib_without_relaxing_tool_bounds() {
    let mut c = started();
    let content = json!([{"type":"text","text":"p".repeat(PROMPT_BYTES),"text_elements":[]},{"type":"image","detail":null,"url":"data:image/png;base64,AAAA"}]);
    c.prompt = Some(content.clone());
    let raw = serde_json::to_vec(&notice(
        "item/started",
        json!({"id":"user1","type":"userMessage","content":content}),
    ))
    .unwrap();
    assert!(raw.len() > MAX_JSON_BYTES);
    let parsed = c.envelope(&raw).unwrap();
    c.accept(parsed).unwrap();
    let mut too_large = call_item();
    too_large["arguments"]["path"] = json!("x".repeat(MAX_JSON_BYTES));
    assert!(started().accept(notice("item/started", too_large)).is_err());
}

#[test]
fn completed_status_alone_cannot_claim_a_successful_answer() {
    let terminal = json!({"method":"turn/completed","params":{"threadId":"thread1","turn":{"id":"turn1","status":"completed","error":null}}});
    assert!(started().accept(terminal.clone()).is_err());
    let mut c = started();
    c.final_text = Some("done".into());
    c.items.insert(
        "pending".into(),
        Item {
            kind: "reasoning".into(),
            completed: false,
        },
    );
    assert!(c.accept(terminal).is_err());
}

#[tokio::test]
async fn preplanted_config_or_catalog_hardlinks_are_rejected_before_initialization() {
    for target in ["config", "catalog"] {
        let directory = tempfile::tempdir().unwrap();
        let base =
            crate::private::directory(&directory.path().canonicalize().unwrap().join("private"))
                .unwrap();
        let cwd = crate::private::directory(&base.join("work")).unwrap();
        let profile = crate::private::directory(&base.join("profile")).unwrap();
        let catalog = base.join("catalog.json");
        crate::private::create(&catalog, b"{}").unwrap();
        let config = profile.join("config.toml");
        crate::private::create(&config, configuration(&catalog).unwrap().as_bytes()).unwrap();
        std::fs::hard_link(
            if target == "config" {
                &config
            } else {
                &catalog
            },
            cwd.join("alias"),
        )
        .unwrap();
        let mut c = codec();
        c.options.cwd = cwd;
        c.options.account_home = profile;
        c.options.catalog_path = catalog;
        c.options.admission.catalog_sha256 = crate::digest(b"{}");
        let mut process = StreamProcess::spawn(tokio::process::Command::new("/bin/cat")).unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            c.initialize(&mut process, "fixture"),
        )
        .await;
        assert!(process.join().await);
        assert!(
            matches!(result, Ok(Err(Error::PrivateState))),
            "{target}: initialization must fail before any provider exchange"
        );
    }
}

#[test]
fn rpc_failures_identify_operation_without_exposing_provider_secrets() {
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
        let error = rpc_failure(
            "account/rateLimits/read",
            &json!({
                "code": -32000,
                "message": format!("{message}: Bearer SYNTHETIC_SECRET user@example.invalid"),
                "data": {"token":"SYNTHETIC_DATA_SECRET"}
            }),
        )
        .to_string();
        assert!(error.contains("account/rateLimits/read"));
        assert!(error.contains("-32000"));
        assert!(error.contains(category));
        assert!(!error.contains("SYNTHETIC"));
        assert!(!error.contains("example.invalid"));
    }
}
