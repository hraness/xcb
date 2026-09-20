//! Explicit, credential-free qualification of the installed runtime and the
//! native MCP helper. A local fixture supplies a loopback-only fake backend.
use super::*;
use crate::{private, sandbox};
use serde::Deserialize;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::process::Command;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    directory: PathBuf,
    provider: PathBuf,
    helper: PathBuf,
    helper_sha256: String,
    port: u16,
    scenario: Scenario,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    Broker,
    Exec,
    Write,
    ConfigWrite,
    Webfetch,
}

#[tokio::test]
#[ignore = "requires exact installed Devin, freshly built native helper, and synthetic loopback fixture"]
async fn installed_runtime_uses_native_broker_under_production_profile() {
    // A harness watchdog must request shutdown rather than terminate this
    // owner before its independent provider process group has joined.
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let spec_path =
        std::env::var_os("XCB_DEVIN_FIXTURE_SPEC").expect("explicit synthetic fixture spec");
    let spec: Spec =
        serde_json::from_slice(&private::read(Path::new(&spec_path), 8192).unwrap()).unwrap();
    assert_ne!(spec.port, 0);
    assert_eq!(
        crate::process::executable_digest(&spec.provider).unwrap(),
        super::super::super::config::BINARY_SHA256
    );
    assert_eq!(
        crate::process::executable_digest(&spec.helper).unwrap(),
        spec.helper_sha256
    );
    let directory = private::directory(&spec.directory).unwrap();
    let scratch = private::directory(&directory.join("scratch")).unwrap();
    let home = private::directory(&scratch.join("home")).unwrap();
    let cwd = private::directory(&scratch.join("work")).unwrap();
    private::directory(&home.join("tmp")).unwrap();
    let config_dir = private::directory(&home.join(".config/devin")).unwrap();
    let workspace_dir = private::directory(&directory.join("consumer")).unwrap();
    let protected = private::directory(&directory.join("persistent-auth")).unwrap();
    for (path, canary) in [
        (protected.join("secret.ipynb"), "SYNTHETIC_ACCOUNT_CANARY"),
        (
            workspace_dir.join("secret.ipynb"),
            "SYNTHETIC_WORKSPACE_CANARY",
        ),
    ] {
        private::create(&path, &serde_json::to_vec(&json!({"nbformat":4,"nbformat_minor":0,"metadata":{},"cells":[{"cell_type":"code","metadata":{},"source":[canary],"outputs":[],"execution_count":null}]})).unwrap()).unwrap();
    }
    std::os::unix::fs::symlink(
        workspace_dir.join("secret.ipynb"),
        home.join("workspace-link.ipynb"),
    )
    .unwrap();
    std::os::unix::fs::symlink("/dev/fd/0", home.join("stdin-link.ipynb")).unwrap();
    let coordination = private::directory(&directory.join("coordination")).unwrap();
    let workspace =
        broker::Workspace::open_with_coordination(&workspace_dir, &coordination).unwrap();
    let socket = directory.join("mcp.sock");
    let bridge = DevinBridge::bind(&socket).unwrap();
    private::create(
        &config_dir.join("config.json"),
        &serde_json::to_vec(&super::super::super::config::configuration()).unwrap(),
    )
    .unwrap();
    private::create(
        &config_dir.join("mcp_config.json"),
        &serde_json::to_vec(&bridge.configuration(&spec.helper).unwrap()).unwrap(),
    )
    .unwrap();
    let production = sandbox::devin_seatbelt(
        &spec.provider,
        &spec.helper,
        &scratch,
        &home,
        &config_dir,
        Some(&socket),
    )
    .unwrap();
    let external = "(allow network-outbound (literal \"/private/var/run/mDNSResponder\") (literal \"/private/var/run/syslog\") (remote tcp \"*:443\"))";
    assert_eq!(production.matches(external).count(), 1);
    let policy = production.replace(
        external,
        &format!(
            "(allow network-outbound (remote tcp \"localhost:{}\"))",
            spec.port
        ),
    );
    let policy_path = directory.join("sandbox.sb");
    private::create(&policy_path, policy.as_bytes()).unwrap();
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-f")
        .arg(policy_path)
        .arg(&spec.provider)
        .arg("--config")
        .arg(config_dir.join("config.json"))
        .args(["--permission-mode", "auto", "acp"])
        .env_clear()
        .envs(crate::process::environment(&home))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("WINDSURF_API_KEY", "synthetic-not-a-secret")
        .env(
            "WINDSURF_API_SERVER_URL",
            format!("http://127.0.0.1:{}", spec.port),
        )
        .current_dir(&cwd);
    let model = ModelChoice {
        provider: Provider::Devin,
        id: Id::new("swe-1-6-fast").unwrap(),
        label: "Synthetic fixture".into(),
        mode: Mode::Fixed,
        resolved: None,
        effort: None,
        observed_at_ms: 1,
    };
    let mut codec = DevinProtocol::new(
        DevinOptions {
            cwd,
            model,
            tools: true,
            metadata_only: false,
        },
        Some(bridge),
    )
    .unwrap();
    let mut process = StreamProcess::spawn(command).unwrap();
    let mut recorded = Vec::new();
    let mut denials = 0usize;
    let outcome = tokio::select! {
    result=tokio::time::timeout(Duration::from_secs(30),async {
        let models=codec.initialize(&mut process,"Offline synthetic qualification; only the xcb broker is available.").await?;
        require(models.is_empty(),"fake backend catalogue")?;
        codec.start(&mut process,Prompt{text:"Perform the synthetic broker fixture.".into(),images:vec![crate::protocol::ImageInput {media_type:"image/png".into(),base64:"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into()}]}).await?;
        let mut admitted=false;
        for _ in 0..1024 {
            let batch=codec.next(&mut process).await?;
            for event in batch.events {
                match event {
                    Event::Ready{..}=>{require(!admitted,"duplicate fixture admission")?;admitted=true;},
                    Event::Tool{id,name,arguments}=>{
                        require(admitted,"fixture tool before admission")?;recorded.push(json!({"name":name,"arguments":arguments}));
                        let value=workspace.call(&name,&arguments)?;
                        codec.reply(&mut process,&id,json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}],"isError":false})).await?;
                    }
                    Event::Result{terminal,text,..}=>{require(admitted,"fixture result before admission")?;return Ok::<_,Error>((terminal,text));},
                    Event::Attention=>denials += 1,
                    _=>(),
                }
            }
        }
        Err(Error::Protocol("synthetic fixture frame limit"))
    })=>result,
    _=terminate.recv()=>Ok(Err(Error::Protocol("synthetic fixture watchdog"))),
    };
    let joined = process.join().await;
    let bridge_joined = codec.shutdown().await;
    let status = match &outcome {
        Ok(Ok(_)) => "passed",
        Ok(Err(_)) => "protocol_failed",
        Err(_) => "deadline",
    };
    let native_calls: Vec<_> = codec
        .calls
        .values()
        .filter(|call| !codec.broker_names.contains(&call.name))
        .map(|call| json!({"name":call.name,"finished":call.finished,"approved":call.approved}))
        .collect();
    let evidence = json!({"status":status,"process_joined":joined,"bridge_joined":bridge_joined,"provider_sha256":super::super::super::config::BINARY_SHA256,"helper_sha256":spec.helper_sha256,"production_policy_sha256":crate::digest(&production),"fixture_policy_sha256":crate::digest(&policy),"calls":recorded,"native_calls":native_calls,"denials":denials,"image_prompt":true,"mcp_proposed_version":codec.mcp_proposed_version,"mcp_metadata_seen":codec.mcp_metadata_seen});
    private::create(
        &directory.join("native-evidence.json"),
        &serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    assert!(
        joined && bridge_joined,
        "all owned work must stop before fixture completion"
    );
    let (terminal, text) = outcome
        .expect("fixture deadline")
        .expect("native fixture protocol");
    if matches!(spec.scenario, Scenario::Broker) {
        assert_eq!(terminal, Terminal::Completed);
        assert_eq!(text, "SYNTHETIC_COMPLETE");
        assert_eq!(
            workspace.read("fixture.txt").unwrap().text,
            "SYNTHETIC_BROKER_WRITE"
        );
        assert_eq!(recorded.len(), 2);
        assert_eq!(codec.calls.len(), 8);
        for (index, (name, arguments)) in [
            (
                "notebook_read",
                json!({"notebook_path":protected.join("secret.ipynb")}),
            ),
            (
                "notebook_read",
                json!({"notebook_path":workspace_dir.join("secret.ipynb")}),
            ),
            (
                "notebook_read",
                json!({"notebook_path":home.join("workspace-link.ipynb")}),
            ),
            (
                "notebook_read",
                json!({"notebook_path":home.join("stdin-link.ipynb")}),
            ),
            (
                "read",
                json!({"file_path":workspace_dir.join("secret.ipynb")}),
            ),
            ("mcp_list_tools", json!({"server_name":"xcb"})),
        ]
        .into_iter()
        .enumerate()
        {
            let call = codec
                .calls
                .get(&format!("synthetic-call-{}", index + 1))
                .unwrap();
            assert_eq!(call.name, name);
            assert_eq!(call.arguments, arguments);
            assert!(call.finished && !call.approved);
        }
    } else {
        // Some denied native tools end the provider turn immediately. Each
        // receives its own disposable process so that denial cannot skip the
        // later probes. An observed failed tool is required even on early end.
        assert!(matches!(
            terminal,
            Terminal::Completed | Terminal::Cancelled
        ));
        assert!(text.is_empty() || text == "SYNTHETIC_COMPLETE");
        assert!(recorded.is_empty());
        let (expected, arguments) = match spec.scenario {
            Scenario::Exec => (
                "exec",
                json!({"command":format!("touch '{}'", home.join("exec-result.txt").display())}),
            ),
            Scenario::Write => (
                "write",
                json!({"file_path":workspace_dir.join("native-write.txt"),"content":"FORBIDDEN"}),
            ),
            Scenario::ConfigWrite => (
                "write",
                json!({"file_path":config_dir.join("config.json"),"content":"{}"}),
            ),
            Scenario::Webfetch => (
                "webfetch",
                json!({"url":format!("http://127.0.0.1:{}/should-not-fetch",spec.port)}),
            ),
            Scenario::Broker => unreachable!(),
        };
        assert_eq!(codec.calls.len(), 1);
        let call = codec.calls.get("synthetic-call-1").unwrap();
        assert_eq!(call.name, expected);
        assert_eq!(call.arguments, arguments);
        assert!(call.finished && !call.approved);
    }
    assert!(!workspace_dir.join("native-write.txt").exists());
    assert!(!home.join("exec-result.txt").exists());
    assert_eq!(
        private::read(&config_dir.join("config.json"), 8192).unwrap(),
        serde_json::to_vec(&super::super::super::config::configuration()).unwrap()
    );
    assert_eq!(
        std::fs::metadata(config_dir.join("config.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
}
