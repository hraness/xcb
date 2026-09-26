use crate::{Error, Result, process::Pin};
use serde_json::{Value, json};
use xcb_core::Provider;

pub const VERSION: &str = "3000.11.3";
pub const BINARY_SHA256: &str = "7ef3859e68d4eabc0115e51898fcd4eab1edde753c27a472349ef551180b38ff";
/// Independently reviewed version/digest pairs. Preserve admitted deployments
/// when a new build passes the same unchanged native inventory and boundary.
const REVIEWED_BUILDS: &[(&str, &str)] = &[
    (VERSION, BINARY_SHA256),
    (
        "3000.11.1",
        "1327c9ff28ec0799e29baa1fe0eeceba2a7c8965123d49058845b6746fea7940",
    ),
    (
        "3000.10.31",
        "4cd4d2e242ed78443fe26d6f55382fd888ece0ac4e80b205106f8f1bf2b0cfa4",
    ),
];
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
    REVIEWED_BUILDS
        .iter()
        .any(|(reviewed, _)| *reviewed == version)
}

/// Artifact admission only. The caller must also require the current host's
/// confinement and provider qualification receipts before activating a route.
pub fn runtime_admitted(pin: &Pin) -> Result<()> {
    if pin.provider != Provider::Devin
        || !REVIEWED_BUILDS
            .iter()
            .any(|(version, sha256)| *version == pin.version && *sha256 == pin.sha256)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_requires_reviewed_pairs_and_preserves_prior_builds() {
        for &(version, sha256) in REVIEWED_BUILDS {
            let mut pin = Pin {
                provider: Provider::Devin,
                executable: "/synthetic/devin".into(),
                sha256: sha256.into(),
                version: version.into(),
                host_sha256: "0".repeat(64),
                observed_at_ms: 1,
            };
            assert!(version_admitted(version));
            assert!(runtime_admitted(&pin).is_ok());
            pin.sha256 = "0".repeat(64);
            assert!(runtime_admitted(&pin).is_err());
            for &(other_version, other_sha256) in REVIEWED_BUILDS {
                if other_version != version {
                    pin.sha256 = other_sha256.into();
                    assert!(
                        runtime_admitted(&pin).is_err(),
                        "reviewed digests cannot be mixed with another version"
                    );
                }
            }
            pin.sha256 = sha256.into();
            pin.provider = Provider::Claude;
            assert!(runtime_admitted(&pin).is_err());
        }
        assert!(!version_admitted("3000.11.2"));
    }
}
