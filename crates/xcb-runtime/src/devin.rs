//! Exact-build Devin ACP integration. The host owns credentials, process
//! custody, OS isolation and broker effects; this codec owns only wire state.
pub mod auth;
mod bridge;
mod config;
mod wire;

#[cfg(target_os = "macos")]
pub(crate) use bridge::DevinBridge;
pub use bridge::broker_stdio;
pub use config::{
    BINARY_SHA256, NATIVE_TOOLS, VERSION, configuration, runtime_admitted, version_admitted,
};
#[cfg(target_os = "macos")]
pub(crate) use wire::DevinOptions;
pub(crate) use wire::DevinProtocol;
pub use wire::parse_models;
