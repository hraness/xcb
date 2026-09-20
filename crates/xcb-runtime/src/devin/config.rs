use crate::{Error, Result, process::Pin};
use serde_json::{Value, json};
use xcb_core::Provider;

pub const VERSION: &str = "3000.10.31";
pub const BINARY_SHA256: &str = "4cd4d2e242ed78443fe26d6f55382fd888ece0ac4e80b205106f8f1bf2b0cfa4";
/// Exact normal-mode inventory captured from this binary's inference request.
/// Presence is not a grant: the OS profile exposes no consumer workspace or
/// persistent account files, and ACP approvals are restricted to our broker.
pub const NATIVE_TOOLS: &[&str] = &[
    "edit",
    "exec",
    "find_file_by_name",
    "get_output",
    "grep",
    "kill_shell",
    "mcp_call_tool",
    "mcp_list_servers",
    "mcp_list_tools",
    "mcp_read_resource",
    "notebook_edit",
    "notebook_read",
    "read",
    "request_scope",
    "skill",
    "todo_write",
    "webfetch",
    "write",
    "write_to_process",
];

pub fn version_admitted(version: &str) -> bool {
    version == VERSION
}

/// Artifact admission only. The caller must also require the current host's
/// confinement and provider qualification receipts before activating a route.
pub fn runtime_admitted(pin: &Pin) -> Result<()> {
    if pin.provider != Provider::Devin
        || !version_admitted(&pin.version)
        || pin.sha256 != BINARY_SHA256
    {
        return Err(Error::Unavailable(
            "Devin build has no exact runtime qualification",
        ));
    }
    Ok(())
}

pub fn configuration() -> Value {
    json!({
        "auto_update":false,"subagents_enabled":false,"notify":"never",
        "read_config_from":{"claude":false,"cursor":false,"windsurf":false,
            "agents_standard":false,"opencode":false,"zed":false,"copilot":false},
        "permissions":{"allow":[],"ask":[],"deny":NATIVE_TOOLS.iter()
            .copied().filter(|n| !n.starts_with("mcp_")).chain(["glob"]).collect::<Vec<_>>()}
    })
}
