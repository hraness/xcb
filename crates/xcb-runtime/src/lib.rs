pub mod application;
mod application_qualification;
pub mod attachments;
pub mod auth;
pub mod broker;
pub mod claude;
mod claude_protocol;
pub mod codex;
pub mod config;
pub mod context;
mod coordination;
pub mod devin;
pub mod egress;
pub mod exports;
pub mod hooks;
pub mod jev;
pub mod judge;
pub mod kernel;
pub mod panes;
pub mod private;
pub mod process;
mod protocol;
#[cfg(any(test, target_os = "macos"))]
mod public_ca;
pub mod qualification;
pub mod runner;
pub mod sandbox;
pub mod store;
pub mod summary;

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
    #[error("state must be an owned physical directory with private permissions")]
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
}
pub type Result<T> = std::result::Result<T, Error>;

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
