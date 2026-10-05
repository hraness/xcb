use crate::{Error, Result};
use serde_json::{Value, json};
use std::sync::LazyLock;
use xcb_core::Provider;

static INVENTORY: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("provider-methods.json")).expect("checked provider inventory")
});

pub fn describe(provider: Provider) -> Result<Value> {
    let groups = match provider {
        Provider::Codex => INVENTORY["codex"]["methods"].as_object().cloned().unwrap(),
        Provider::Claude => [("Query".to_owned(), INVENTORY["claude"]["Query"].clone())]
            .into_iter()
            .collect(),
        Provider::Devin => return Err(Error::Unavailable("Devin execution is retired")),
    };
    let mut methods = Vec::new();
    for (direction, names) in groups {
        for name in names.as_array().unwrap() {
            let method = name.as_str().unwrap();
            let status = status(provider, &direction, method);
            methods.push(json!({"method":method,"direction":direction,"status":status}));
        }
    }
    Ok(json!({
        "version":1, "provider":provider, "scope":"taskAdapter",
        "arbitraryProviderCalls":false, "coverageMeaning":"accountedForNotAllEnabled",
        "codexSchemaSha256":if provider == Provider::Codex {Some(crate::codex::SCHEMA_SHA256)} else {None},
        "claudeSdkVersion":if provider == Provider::Claude {INVENTORY["claude"]["sdkVersion"].as_str()} else {None},
        "methods":methods
    }))
}

fn status(provider: Provider, direction: &str, method: &str) -> &'static str {
    match provider {
        Provider::Claude => match method {
            "accountInfo"
            | "initializationResult"
            | "supportedAgents"
            | "supportedCommands"
            | "supportedModels" => "startup",
            "getContextUsage" | "mcpServerStatus" => "metadataProbeDiagnostic",
            "usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET" => "startup",
            "close" | "interrupt" | "streamInput" => "execution",
            "setModel" | "readFile" => "hostOwnedNotProviderPassthrough",
            _ => "notImplemented",
        },
        Provider::Codex => match direction {
            "ClientNotification" => "startup",
            "ClientRequest" => match method {
                "initialize"
                | "account/read"
                | "account/rateLimits/read"
                | "config/read"
                | "model/list" => "startup",
                "thread/start" | "turn/start" | "turn/interrupt" => "execution",
                "mcpServerStatus/list" => "grantedConnectorDiagnostic",
                "account/rateLimitResetCredit/consume" => "quotaRefreshOnlyNotInspection",
                "thread/resume" | "turn/steer" | "fs/readFile" | "fs/writeFile"
                | "fs/readDirectory" | "fs/remove" | "command/exec" => {
                    "hostOwnedNotProviderPassthrough"
                }
                _ => "notImplemented",
            },
            "ServerRequest" => match method {
                "item/tool/call" => "execution",
                _ => "rejected",
            },
            "ServerNotification" => match method {
                "account/updated"
                | "account/rateLimits/updated"
                | "error"
                | "remoteControl/status/changed"
                | "thread/started"
                | "thread/settings/updated"
                | "thread/status/changed"
                | "thread/tokenUsage/updated"
                | "turn/started"
                | "turn/completed"
                | "item/started"
                | "item/completed"
                | "item/agentMessage/delta"
                | "item/reasoning/summaryTextDelta"
                | "item/reasoning/textDelta"
                | "item/reasoning/summaryPartAdded"
                | "item/autoApprovalReview/started"
                | "item/autoApprovalReview/completed"
                | "item/mcpToolCall/progress"
                | "mcpServer/startupStatus/updated" => "validatedObservation",
                _ if ["model/", "account/", "item/", "turn/"]
                    .iter()
                    .any(|prefix| method.starts_with(prefix)) =>
                {
                    "rejected"
                }
                _ => "ignoredDiagnosticOnly",
            },
            _ => "rejected",
        },
        Provider::Devin => "rejected",
    }
}

pub(crate) fn codex_rpc_supported(method: &str) -> bool {
    matches!(
        status(Provider::Codex, "ClientRequest", method),
        "startup" | "execution" | "grantedConnectorDiagnostic" | "quotaRefreshOnlyNotInspection"
    )
}

#[cfg(test)]
pub(crate) fn codex_methods(direction: &str) -> impl Iterator<Item = &'static str> {
    INVENTORY["codex"]["methods"][direction]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn catalog_accounts_for_every_checked_method_without_enabling_passthrough() {
        assert_eq!(
            INVENTORY["codex"]["schemaSha256"],
            crate::codex::SCHEMA_SHA256
        );
        for (direction, count) in [
            ("ClientRequest", 167),
            ("ClientNotification", 1),
            ("ServerRequest", 11),
            ("ServerNotification", 83),
        ] {
            let methods: Vec<_> = codex_methods(direction).collect();
            assert_eq!(methods.len(), count);
            assert_eq!(
                methods.iter().copied().collect::<BTreeSet<_>>().len(),
                count
            );
            assert!(methods.windows(2).all(|pair| pair[0] < pair[1]));
        }
        for (provider, count) in [(Provider::Claude, 29), (Provider::Codex, 262)] {
            let result = describe(provider).unwrap();
            assert_eq!(result["arbitraryProviderCalls"], false);
            assert_eq!(result["methods"].as_array().unwrap().len(), count);
            assert!(
                result["methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["method"].is_string() && row["status"].is_string())
            );
        }
        assert!(describe(Provider::Devin).is_err());
    }

    #[test]
    fn protocol_rpc_names_cannot_silently_escape_the_reviewed_inventory() {
        let names: BTreeSet<_> = codex_methods("ClientRequest").collect();
        for method in [
            "initialize",
            "account/read",
            "account/rateLimits/read",
            "config/read",
            "model/list",
            "thread/start",
            "turn/start",
            "turn/interrupt",
            "mcpServerStatus/list",
            "account/rateLimitResetCredit/consume",
        ] {
            assert!(names.contains(method), "{method}");
        }
        for method in [
            "thread/fork",
            "thread/revert",
            "thread/settings/update",
            "turn/settings/update",
            "config/value/write",
            "config/batchWrite",
            "config/mcpServer/reload",
            "process/spawn",
            "remoteControl/enable",
            "thread/approveGuardianDeniedAction",
            "mcpServer/tool/call",
        ] {
            assert_eq!(
                status(Provider::Codex, "ClientRequest", method),
                "notImplemented"
            );
        }
    }
}
