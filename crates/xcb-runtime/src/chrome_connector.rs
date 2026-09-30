//! The installed Claude browser extension is a shared MCP capability. Its
//! native host keeps browser custody and permission prompts. Only the selected
//! xcb Claude account supplies authentication, under its exclusive lease;
//! inherited settings and permission overrides are never forwarded.
use crate::{
    Error, Result,
    capabilities::{CapabilityFeature, CapabilityServer, CapabilityTransport},
    process::Pin,
};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// Explicit connection setup intentionally preserves the existing browser
/// group, or the initial blank tab created for that persistent connection.
/// Browser context is inspected only for success and is never returned.
pub async fn setup(
    state_root: &Path,
    server: CapabilityServer,
    workspace: &Path,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if *cancel.borrow() {
        return Err(Error::Unavailable("browser setup cancelled"));
    }
    server.validate()?;
    if server.name != SERVER_NAME {
        return Err(Error::Protocol("browser setup connector"));
    }
    let account = server.credential_account.clone().ok_or(Error::Unavailable(
        "select a Claude account before browser setup",
    ))?;
    let store = std::sync::Arc::new(crate::store::Store::open(state_root)?);
    verify_credential_target(&server, &store, workspace)?;
    if store.account(&account)?.provider != xcb_core::Provider::Claude {
        return Err(Error::Unavailable(
            "browser setup requires a Claude account",
        ));
    }
    let run = store.prepare_probe(&account, None, crate::now_ms())?;
    let manager = crate::capabilities::CapabilityManager::new(
        crate::capabilities::CapabilityConfig {
            servers: vec![server],
        },
        store.clone(),
        run.clone(),
        workspace.to_path_buf(),
    );
    let mut manager = match manager {
        Ok(manager) => manager,
        Err(error) => {
            store.settle(&run, xcb_core::session::State::Idle, crate::now_ms())?;
            return Err(error);
        }
    };
    let result = tokio::select! {
        biased;
        _=async {while !*cancel.borrow() {if cancel.changed().await.is_err(){break;}}}=>Err(Error::Unavailable("browser setup cancelled")),
        result=manager.bootstrap_chrome()=>result,
    };
    manager.finish_chrome_setup().await?;
    result
}

pub(crate) fn setup_succeeded(result: &Value) -> bool {
    if result["isError"] == true {
        return false;
    }
    let mut contexts = texts(result).filter_map(|text| serde_json::from_str::<Value>(text).ok());
    let Some(context) = contexts.next() else {
        return false;
    };
    contexts.next().is_none()
        && context["tabGroupId"].as_u64().is_some_and(|id| id > 0)
        && context["availableTabs"].as_array().is_some_and(|tabs| {
            !tabs.is_empty()
                && tabs.len() <= 256
                && tabs
                    .iter()
                    .all(|tab| tab["tabId"].as_u64().is_some_and(|id| id > 0))
        })
}

pub(crate) fn setup_was_refused(result: &Value) -> bool {
    policy_denied(result)
        || texts(result).any(|text| {
            text.starts_with("Browser extension is not connected.")
                || text.starts_with("Browser extension is not connected:")
        })
}

pub const SERVER_NAME: &str = "claude_browser";
pub const ADMITTED_VERSION: &str = "2.1.285";
const TOOLS: [&str; 20] = [
    "javascript_tool",
    "read_page",
    "find",
    "form_input",
    "computer",
    "navigate",
    "resize_window",
    "gif_creator",
    "upload_image",
    "get_page_text",
    "tabs_context_mcp",
    "tabs_create_mcp",
    "tabs_close_mcp",
    "read_console_messages",
    "read_network_requests",
    "shortcuts_list",
    "file_upload",
    "switch_browser",
    "list_connected_browsers",
    "select_browser",
];

/// Register the exact inspected native MCP implementation. An upstream change
/// requires reviewing its permission and transport contract before admission.
/// The pinned executable is already kept in private xcb custody; each launch
/// snapshots it again and runs with a disposable HOME and empty environment.
pub fn registration(pin: &Pin, consumer_workspace: &Path) -> Result<CapabilityServer> {
    if pin.provider != xcb_core::Provider::Claude || pin.version != ADMITTED_VERSION {
        return Err(Error::Unavailable(
            "the shared browser connector requires the inspected Claude Code 2.1.285 runtime",
        ));
    }
    pin.verify()?;
    if pin
        .executable
        .starts_with(xcb_core::canonical(consumer_workspace)?)
    {
        return Err(Error::Unavailable(
            "browser connector cannot load from the task workspace",
        ));
    }
    let server = CapabilityServer {
        credential_account: None,
        name: SERVER_NAME.into(),
        transport: CapabilityTransport::Shared,
        executable: pin.executable.clone(),
        sha256: pin.sha256.clone(),
        bundles: vec![],
        args: vec!["--claude-in-chrome-mcp".into()],
        env: vec![],
        environment: BTreeMap::from([(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        )]),
        tools: Some(TOOLS.iter().map(|tool| (*tool).into()).collect()),
        shutdown_tool: None,
        features: vec![CapabilityFeature::Browser, CapabilityFeature::Computer],
        timeout_ms: 120_000,
    };
    server.validate()?;
    Ok(server)
}

pub(crate) fn verify_credential_target(
    server: &CapabilityServer,
    store: &crate::store::Store,
    workspace: &Path,
) -> Result<()> {
    let pin = Pin::load(store.root(), xcb_core::Provider::Claude)?;
    let admitted = registration(&pin, workspace)?;
    if server.executable != admitted.executable
        || server.sha256 != admitted.sha256
        || server.args != admitted.args
        || !server.bundles.is_empty()
        || !server.env.is_empty()
        || server.environment != admitted.environment
        || server.shutdown_tool.is_some()
        || !server.tools.as_ref().is_some_and(|tools| {
            tools.iter().all(|tool| TOOLS.contains(&tool.as_str()))
                && (!tools.iter().any(|tool| tool == "tabs_create_mcp")
                    || tools.iter().any(|tool| tool == "tabs_close_mcp"))
        })
    {
        return Err(Error::Unavailable(
            "browser account credentials require the admitted unmodified Claude browser connector",
        ));
    }
    Ok(())
}

/// The admitted upstream version encodes extension denials as MCP tool errors
/// with these anchored prefixes (its own telemetry classifier does the same).
/// Success text from a page is never a policy decision. Connection errors are
/// separate and cannot accidentally be promoted to user permission denials.
pub(crate) fn policy_denied(result: &Value) -> bool {
    result["isError"] == true
        && result["content"].as_array().is_some_and(|content| {
            content.iter().any(|item| {
                item["type"] == "text"
                    && item["text"].as_str().is_some_and(|text| {
                        [
                            "Permission denied by user",
                            "Permission denied for JavaScript execution",
                            "Permission denied for this action",
                            "Permission denied for reading page content",
                            "Permission required but no handler",
                            "Cannot access this page. Claude cannot assist",
                        ]
                        .iter()
                        .any(|prefix| text.starts_with(prefix))
                    })
            })
        })
}

/// Group creation does not report which tabs were actually created. Batches
/// and shortcuts also hide creation outcomes, so those tools are not admitted.
pub(crate) fn validate_call(tool: &str, arguments: &Value) -> Result<()> {
    if tool == "tabs_context_mcp"
        && arguments
            .get("createIfEmpty")
            .is_some_and(|value| value != false)
    {
        return Err(Error::Unavailable(
            "run xcb tools setup-browser to establish the persistent browser connection before task use",
        ));
    }
    if tool == "navigate" && arguments.get("tabId").and_then(Value::as_u64).is_none() {
        return Err(Error::Unavailable(
            "browser navigation requires an existing tab ID",
        ));
    }
    Ok(())
}

fn texts(result: &Value) -> impl Iterator<Item = &str> {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "text")
        .filter_map(|item| item["text"].as_str())
}

pub(crate) fn created_tab(result: &Value) -> Result<Option<u64>> {
    if result["isError"] == true {
        if group_missing(result) {
            return Ok(None);
        }
        return Err(Error::Protocol(
            "browser tab creation failed with an uncertain outcome",
        ));
    }
    if texts(result).any(|text| text.starts_with("Browser extension is not connected.")) {
        return Ok(None);
    }
    let mut ids = texts(result).filter_map(|text| {
        text.strip_prefix("Created new tab. Tab ID: ")
            .and_then(|id| id.parse::<u64>().ok())
            .filter(|id| *id > 0)
    });
    let id = ids.next().ok_or(Error::Protocol(
        "browser tab creation returned no custody identifier",
    ))?;
    if ids.next().is_some() {
        return Err(Error::Protocol(
            "browser tab creation returned ambiguous custody identifiers",
        ));
    }
    Ok(Some(id))
}

pub(crate) fn group_missing(result: &Value) -> bool {
    result["isError"] == true
        && texts(result).any(|text| {
            [
                "No tab group exists for this session yet.",
                "This session's tab group no longer exists (tabs were closed).",
                "No MCP tab group exists.",
                "The MCP tab group no longer exists (tabs were closed).",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix))
        })
}

pub(crate) fn closed_tab(result: &Value, id: u64) -> bool {
    if result["isError"] == true {
        texts(result).any(|text| {
            text.starts_with(&format!(
                "Tab {id} does not exist (may have already been closed)."
            ))
        })
    } else {
        texts(result).any(|text| text.starts_with(&format!("Closed tab {id}. ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn denial_requires_error_from_trusted_bridge_not_successful_page_content() {
        for prefix in [
            "Permission denied by user. Domain transition denied",
            "Permission denied for JavaScript execution",
            "Permission denied for this action",
            "Permission denied for reading page content",
            "Permission required but no handler",
            "Cannot access this page. Claude cannot assist",
        ] {
            let mut result = json!({"isError":true,"content":[{"type":"text","text":prefix}]});
            assert!(policy_denied(&result));
            result["isError"] = false.into();
            assert!(!policy_denied(&result));
        }
        for text in [
            "Browser extension is not connected.",
            "The tool call was never delivered to the Chrome extension.",
            "User page says: Permission denied by user",
            "No element found with reference",
        ] {
            assert!(!policy_denied(
                &json!({"isError":true,"content":[{"type":"text","text":text}]})
            ));
        }
    }

    #[test]
    fn registration_pins_native_mcp_without_provider_credentials_or_permission_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let (executable, sha256) = crate::process::host_identity().unwrap();
        let pin = Pin {
            provider: xcb_core::Provider::Claude,
            executable,
            sha256: sha256.clone(),
            host_sha256: sha256,
            version: ADMITTED_VERSION.into(),
            observed_at_ms: 1,
        };
        let server = registration(&pin, directory.path()).unwrap();
        assert_eq!(server.transport, CapabilityTransport::Shared);
        assert_eq!(server.args, ["--claude-in-chrome-mcp"]);
        assert_eq!(server.tools.as_ref().unwrap().len(), 20);
        assert!(server.env.is_empty());
        assert!(
            !server
                .environment
                .contains_key("CLAUDE_CHROME_PERMISSION_MODE")
        );
        assert!(!server.environment.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        let mut drift = pin.clone();
        drift.version = "2.1.286".into();
        assert!(registration(&drift, directory.path()).is_err());
        assert!(registration(&pin, pin.executable.parent().unwrap()).is_err());
    }

    #[test]
    fn creation_custody_uses_only_explicit_created_id_and_bounded_close_proof() {
        assert!(validate_call("tabs_context_mcp", &json!({"createIfEmpty":true})).is_err());
        assert!(validate_call("tabs_context_mcp", &json!({"createIfEmpty":false})).is_ok());
        assert!(validate_call("navigate", &json!({"url":"https://example.test"})).is_err());
        let created = json!({"content":[{"type":"text","text":"Created new tab. Tab ID: 123"},{"type":"text","text":"User-owned tab ID: 456"}]});
        assert_eq!(created_tab(&created).unwrap(), Some(123));
        assert!(
            created_tab(&json!({"content":[{"type":"text","text":"Existing tab ID: 456"}]}))
                .is_err()
        );
        assert!(
            created_tab(
                &json!({"isError":true,"content":[{"type":"text","text":"Failed to create tab"}]})
            )
            .is_err()
        );
        assert_eq!(
            created_tab(
                &json!({"content":[{"type":"text","text":"Browser extension is not connected."}]})
            )
            .unwrap(),
            None
        );
        assert!(closed_tab(
            &json!({"content":[{"type":"text","text":"Closed tab 123. Group is now empty (auto-removed)."}]}),
            123
        ));
        assert!(!closed_tab(
            &json!({"content":[{"type":"text","text":"Closed tab 456. Group is now empty (auto-removed)."}]}),
            123
        ));
        assert!(closed_tab(
            &json!({"isError":true,"content":[{"type":"text","text":"Tab 123 does not exist (may have already been closed). Call tabs_context_mcp to see current tabs."}]}),
            123
        ));
    }
}
