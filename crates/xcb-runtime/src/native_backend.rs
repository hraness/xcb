use crate::{Error, Result};
use serde::Serialize;
use xcb_core::{Provider, session::TaskRequirements};

pub const NOT_QUALIFIED: &str = "native provider execution is not yet implemented and qualified; no broker or offline command fallback is permitted for this task";

pub const ACCEPTANCE_CASES: &[&str] = &[
    "native_tool_inventory",
    "workspace_read_write_confinement",
    "private_account_and_configuration_isolation",
    "native_shell_and_toolchain",
    "network_dns_and_https",
    "git_worktree_and_authorized_remote_effects",
    "provider_approval_readback",
    "denial_stops_continuation_and_failover",
    "cancellation_joins_descendants",
    "uncertain_effect_retains_custody",
    "restart_and_resume_preserve_execution_grant",
    "cross_provider_handoff_preserves_execution_grant",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTransport {
    CodexAppServer,
    ClaudeStreamJson,
    DevinAcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeApproval {
    CodexAutoReview,
    ClaudeAuto,
    DevinProviderReview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeBackendStatus {
    pub provider: Provider,
    pub transport: NativeTransport,
    pub approval: NativeApproval,
    pub implemented: bool,
    pub qualified: bool,
    pub fallback_permitted: bool,
    pub required_cases: &'static [&'static str],
}

pub fn status(provider: Provider) -> NativeBackendStatus {
    let (transport, approval) = match provider {
        Provider::Codex => (
            NativeTransport::CodexAppServer,
            NativeApproval::CodexAutoReview,
        ),
        Provider::Claude => (
            NativeTransport::ClaudeStreamJson,
            NativeApproval::ClaudeAuto,
        ),
        Provider::Devin => (
            NativeTransport::DevinAcp,
            NativeApproval::DevinProviderReview,
        ),
    };
    NativeBackendStatus {
        provider,
        transport,
        approval,
        implemented: false,
        qualified: false,
        fallback_permitted: false,
        required_cases: ACCEPTANCE_CASES,
    }
}

pub fn statuses() -> Vec<NativeBackendStatus> {
    Provider::ALL.into_iter().map(status).collect()
}

pub fn require_execution(requirements: TaskRequirements) -> Result<()> {
    if requirements.native_execution {
        return Err(Error::Unavailable(NOT_QUALIFIED));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_candidates_cover_every_provider_without_claiming_activation() {
        let statuses = statuses();
        assert_eq!(statuses.len(), Provider::ALL.len());
        for (provider, status) in Provider::ALL.into_iter().zip(statuses) {
            assert_eq!(status.provider, provider);
            assert!(!status.implemented && !status.qualified && !status.fallback_permitted);
            assert_eq!(status.required_cases, ACCEPTANCE_CASES);
        }
    }

    #[test]
    fn legacy_execution_is_unchanged_but_native_requests_never_fall_back() {
        assert!(require_execution(TaskRequirements::default()).is_ok());
        for provider in Provider::ALL {
            let requirements = TaskRequirements {
                native_execution: true,
                ..Default::default()
            };
            assert!(requirements.allows(provider));
            assert!(matches!(
                require_execution(requirements.merge(TaskRequirements::default())),
                Err(Error::Unavailable(NOT_QUALIFIED))
            ));
        }
    }

    #[tokio::test]
    async fn native_requirement_is_persisted_and_refused_before_any_run_is_prepared() {
        use crate::{config::Config, runner, store::Store};
        use std::sync::Arc;
        use tokio::sync::watch;
        use xcb_core::{
            Id,
            models::{Mode, ModelChoice},
            session::{Message, Role},
        };

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        for provider in Provider::ALL {
            let account = store.add_account(provider, "Fixture", 1, None).unwrap();
            let model = ModelChoice {
                provider,
                id: Id::new("fixture").unwrap(),
                label: "Fixture".into(),
                mode: Mode::Fixed,
                effort: None,
                resolved: None,
                observed_at_ms: 1,
            };
            let session = store
                .create_managed_session(
                    &account.id,
                    model,
                    &workspace,
                    1,
                    &Id::new(format!("task_{provider}")).unwrap(),
                )
                .unwrap();
            store
                .require_session_capabilities(
                    &session.id,
                    TaskRequirements {
                        native_execution: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            store
                .require_session_capabilities(&session.id, TaskRequirements::default())
                .unwrap();
            let session = store.session(&session.id).unwrap().unwrap();
            assert!(session.requirements.native_execution);
            let (_, cancel) = watch::channel(false);
            let result = runner::run(
                store.clone(),
                runner::RunInput {
                    session,
                    message: Message {
                        id: Id::new("message_fixture").unwrap(),
                        role: Role::User,
                        text: "Run native tools".into(),
                        at_ms: 1,
                        attachments: vec![],
                        provenance: None,
                    },
                    config: Config::default(),
                    pane_generation: false,
                },
                cancel,
                Arc::new(|_| {}),
            )
            .await;
            assert!(matches!(result, Err(Error::Unavailable(NOT_QUALIFIED))));
            assert!(store.unsettled_runs().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn native_routing_and_dry_run_do_not_select_broker_only_accounts() {
        use crate::{config::Config, route, routing, store::Store};
        use std::{collections::BTreeSet, sync::Arc};
        use tokio::sync::watch;

        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&root.join("workspace")).unwrap();
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        let requirements = TaskRequirements {
            native_execution: true,
            ..Default::default()
        };
        let excluded = BTreeSet::new();
        let accounts = BTreeSet::new();
        for provider in Provider::ALL {
            let result = routing::smart_route(
                &store,
                &Config::default(),
                routing::RouteRequest {
                    requirements,
                    task: "Run native tools",
                    required_provider: Some(provider),
                    preferred_provider: None,
                    required_model: None,
                    excluded_routes: &excluded,
                    excluded_accounts: &accounts,
                    account: None,
                },
            )
            .await;
            assert!(matches!(result, Err(Error::Unavailable(NOT_QUALIFIED))));
            for dry_run in [false, true] {
                let request = route::RouteTaskRequest::parse(
                    &serde_json::to_vec(&serde_json::json!({
                        "version":1,
                        "workspace":workspace,
                        "task":"Run native tools",
                        "provider":provider,
                        "requirements":{"native_execution":true},
                        "dryRun":dry_run,
                    }))
                    .unwrap(),
                )
                .unwrap();
                let (_, cancel) = watch::channel(false);
                let result =
                    route::dispatch(store.clone(), request, cancel, Arc::new(|_| {})).await;
                let failure = result.unwrap_err();
                assert_eq!(failure.code, route::RouteCode::Unavailable);
                assert_eq!(failure.joined, Some(true));
                assert_eq!(failure.effects, Some("none"));
                assert!(failure.session.is_none() && failure.route.is_none());
                assert!(store.sessions(10).unwrap().is_empty());
                assert!(store.unsettled_runs().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn provider_specific_approval_contracts_do_not_alias_bypass() {
        assert_eq!(
            status(Provider::Codex).approval,
            NativeApproval::CodexAutoReview
        );
        assert_eq!(
            status(Provider::Claude).approval,
            NativeApproval::ClaudeAuto
        );
        assert_eq!(
            status(Provider::Devin).approval,
            NativeApproval::DevinProviderReview
        );
        assert_ne!(
            status(Provider::Devin).approval,
            status(Provider::Claude).approval
        );
    }
}
