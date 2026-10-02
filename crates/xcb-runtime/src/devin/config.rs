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

/// Admit an exact reviewed pair from the baked release set or the shared
/// catalog. This lets provider updates land without an xcb release while
/// preserving the exact digest and version boundary.
pub fn runtime_admitted_with_catalog(root: &std::path::Path, pin: &Pin) -> Result<()> {
    if crate::catalog::denied(root, &pin.sha256) {
        return Err(Error::Unavailable(
            "Devin build is denied by the reviewed-builds catalog",
        ));
    }
    if runtime_admitted(pin).is_ok()
        || (pin.provider == Provider::Devin
            && crate::catalog::admitted(root, Provider::Devin, &pin.version, &pin.sha256))
    {
        return Ok(());
    }
    Err(Error::Unavailable(
        "Devin build has no exact runtime qualification; run xcb doctor after a reviewed update",
    ))
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

/// Candidate-only configuration: the installed build still exposes `skill`
/// and `mcp_read_resource`, so this has no production activation path.
#[cfg(test)]
pub(super) fn bypass_candidate_configuration() -> Value {
    let disabled: Vec<_> = NATIVE_TOOLS
        .iter()
        .copied()
        .filter(|name| !["mcp_call_tool", "mcp_list_tools", "mcp_list_servers"].contains(name))
        .chain(["glob"])
        .collect();
    json!({
        "auto_update":false,"subagents_enabled":false,"notify":"never",
        "disabled_tools":disabled,
        "read_config_from":{"claude":false,"cursor":false,"windsurf":false,
            "agents_standard":false,"opencode":false,"zed":false,"copilot":false},
        "permissions":{"allow":[],"ask":[],"deny":disabled}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_candidate_requests_native_tools_be_disabled() {
        let config = bypass_candidate_configuration();
        for name in NATIVE_TOOLS.iter().copied().chain(["glob"]) {
            let disabled = !["mcp_call_tool", "mcp_list_tools", "mcp_list_servers"].contains(&name);
            for key in ["/disabled_tools", "/permissions/deny"] {
                assert_eq!(
                    config
                        .pointer(key)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&json!(name)),
                    disabled,
                    "{key}: {name}"
                );
            }
        }
        assert_eq!(config["permissions"]["allow"], json!([]));
        assert_eq!(config["permissions"]["ask"], json!([]));
        assert_eq!(config["subagents_enabled"], false);
        assert!(
            config["read_config_from"]
                .as_object()
                .unwrap()
                .values()
                .all(|enabled| enabled == false)
        );
    }

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

    #[test]
    fn catalog_admission_accepts_exact_updates_and_honors_denials() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        crate::private::directory(&root.join("providers")).unwrap();
        let digest = "a".repeat(64);
        crate::private::create(
            &root.join("providers/catalog.json"),
            serde_json::json!({
                "version": 1,
                "devin": [{"version": "3000.12.0", "sha256": digest.clone()}],
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let pin = Pin {
            provider: Provider::Devin,
            executable: "/synthetic/devin".into(),
            sha256: digest.clone(),
            version: "3000.12.0".into(),
            host_sha256: "0".repeat(64),
            observed_at_ms: 0,
        };
        runtime_admitted_with_catalog(&root, &pin).unwrap();
        let mut unknown = pin.clone();
        unknown.version = "3000.12.1".into();
        assert!(runtime_admitted_with_catalog(&root, &unknown).is_err());

        crate::private::replace(
            &root.join("providers/catalog.json"),
            serde_json::json!({
                "version": 1,
                "devin": [{"version": "3000.12.0", "sha256": digest.clone()}],
                "deny": {"devin": [digest]},
            })
            .to_string()
            .as_bytes(),
            &crate::digest(
                &crate::private::read(&root.join("providers/catalog.json"), 64 * 1024).unwrap(),
            ),
        )
        .unwrap();
        assert!(runtime_admitted_with_catalog(&root, &pin).is_err());
    }
}
