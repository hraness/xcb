//! The provider egress bridge serves a Unix socket into a Linux bwrap
//! namespace. Windows never launches providers, so the bridge cannot start
//! there: this keeps the public shape and refuses.

use crate::{Error, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    path::{Path, PathBuf},
    time::Duration,
};

pub type EgressDialer = std::sync::Arc<dyn Fn(String, u16) -> EgressDialFuture + Send + Sync>;
pub type EgressDialFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<tokio::net::TcpStream>> + Send>>;

pub struct EgressBridgeOptions {
    pub socket_path: PathBuf,
    pub allowlist: Option<BTreeSet<String>>,
    pub max_connections: usize,
    pub idle_timeout: Duration,
    pub allowed_port: u16,
    pub dialer: Option<EgressDialer>,
}
impl EgressBridgeOptions {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            allowlist: None,
            max_connections: 64,
            idle_timeout: Duration::from_secs(30),
            allowed_port: 443,
            dialer: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressBridgeReceipt {
    pub socket_path: PathBuf,
    pub production_qualified: bool,
    pub connections_accepted: u64,
    pub connections_refused: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub listener_closed: bool,
    pub sockets_joined: bool,
    pub socket_removed: bool,
}

/// Never constructed on Windows.
pub struct EgressBridge(Infallible);

impl EgressBridge {
    pub async fn start(_options: EgressBridgeOptions) -> Result<Self> {
        Err(Error::providers_unsupported())
    }
    pub fn socket_path(&self) -> &Path {
        match self.0 {}
    }
    pub fn connections(&self) -> usize {
        match self.0 {}
    }
    pub async fn close(self) -> EgressBridgeReceipt {
        match self.0 {}
    }
}

pub fn write_forwarder_env(_scratch: &Path, _pairs: &BTreeMap<String, String>) -> Result<PathBuf> {
    Err(Error::providers_unsupported())
}

pub async fn run_forwarder(
    _socket: &Path,
    _port: u16,
    _allowed_port: u16,
    _lo_up: Option<&Path>,
    _env_file: Option<&Path>,
    _child: &[String],
) -> Result<i32> {
    Err(Error::providers_unsupported())
}
