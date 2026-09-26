mod agent_overview;
pub mod application;
pub mod application_diagnostic;
mod application_qualification;
pub mod attachments;
pub mod auth;
pub mod broker;
pub mod catalog;
pub mod claude;
mod claude_protocol;
pub mod cloud;
pub mod codex;
pub mod command;
pub mod command_tool;
pub mod config;
pub mod context;
mod coordination;
pub mod devin;
pub mod egress;
pub mod exports;
pub mod habitat_service;
pub mod hooks;
pub mod jev;
pub mod judge;
pub mod kernel;
pub mod managed;
pub mod managed_program;
pub(crate) mod managed_relay;
mod managed_supervisor;
pub mod offers;
pub mod panes;
pub mod private;
pub mod process;
mod protocol;
#[cfg(any(test, target_os = "macos"))]
mod public_ca;
pub mod qualification;
pub mod reflex;
pub mod route;
pub mod routing;
pub mod runner;
pub mod sandbox;
pub mod store;
pub mod summary;
mod task_classifier;
mod transcript;
pub mod update;
mod wire_helpers;
pub mod wordcell;

use sha2::{Digest, Sha256};
use xcb_core::Id;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] xcb_core::Error),
    #[error("local I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Command::spawn failed before a child existed. Post-spawn failures must
    /// never use this variant: callers use it as explicit no-child evidence.
    #[error("provider could not start: {0}")]
    LaunchNotStarted(std::io::Error),
    /// A preparation helper failed to prove shutdown; retain account custody.
    #[error("provider preparation cleanup is unproven; account custody retained")]
    CleanupUnproven,
    #[error("local database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid local record")]
    Json(#[from] serde_json::Error),
    #[error(
        "state must be an owned real directory with private permissions; symlinked, foreign-owned or shared paths are refused"
    )]
    PrivateState,
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("unavailable: {0}")]
    Unavailable(&'static str),
    #[error("provider protocol error: {0}")]
    Protocol(&'static str),
    #[error("Codex {method} failed (RPC {code}): {category}")]
    CodexRpc {
        method: &'static str,
        code: i64,
        category: &'static str,
    },
    #[error("Devin model choices have unsupported shape {shape} (count {count:?})")]
    DevinModelChoices {
        shape: &'static str,
        count: Option<usize>,
    },
    #[error("Devin {method} failed (RPC {code}): {category}")]
    DevinRpc {
        method: &'static str,
        code: i64,
        category: &'static str,
    },
    /// A complete user-facing sentence, printed verbatim — use instead of
    /// `Core(Invalid)` when the text is guidance, not a noun fragment.
    #[error("{0}")]
    Message(&'static str),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Fixed provider failure categories. They are the only provider-derived text
/// that reaches diagnostics, and the runner settles account state from them,
/// so every codec maps its raw errors onto exactly these strings.
pub(crate) mod category {
    pub const AUTHENTICATION: &str = "authentication rejected; reconnect this account";
    pub const CODEX_USAGE_LIMIT: &str = "provider usage limit exceeded";
    pub const DEVIN_RESOURCE_LIMIT: &str = "provider quota or resource limit reached";
    pub const TLS: &str = "TLS certificate or transport failure";
    pub const NETWORK: &str = "provider request or network failure";
}

impl Error {
    /// Account-level classification of a turn that failed with this error.
    /// Only a codec-assigned fixed category can name authentication, quota or
    /// transport; every other error stays `Unknown` so custody is not
    /// released or recovery started on a guess.
    pub(crate) fn failure(&self) -> xcb_core::policy::Failure {
        use xcb_core::policy::Failure;
        let category = match self {
            Error::CodexRpc { category, .. } | Error::DevinRpc { category, .. } => *category,
            Error::Unavailable(text) => *text,
            _ => return Failure::Unknown,
        };
        match category {
            category::AUTHENTICATION => Failure::Authentication,
            category::CODEX_USAGE_LIMIT | category::DEVIN_RESOURCE_LIMIT => Failure::AccountQuota,
            category::TLS | category::NETWORK => Failure::Transport,
            _ => Failure::Unknown,
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
pub fn new_id(prefix: &str) -> Id {
    Id::new(format!("{prefix}_{}", uuid::Uuid::new_v4().simple())).expect("generated identifier")
}
pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

#[cfg(test)]
mod authentication_tests;
