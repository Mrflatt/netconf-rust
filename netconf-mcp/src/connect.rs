use netconf_async::{Connection, NetconfClientResult};

use crate::AllowedNets;

/// Per-call connection arguments from a tool.
#[derive(Debug, Clone)]
pub struct ConnectParams {
    /// Device host. May include `:port` or `[ipv6]:port`.
    pub host: String,
    /// SSH user. `None` lets the connector use env / ssh_config.
    pub username: Option<String>,
    /// SSH password. Last resort; never defaulted by this crate.
    pub password: Option<String>,
    /// Per-RPC timeout in seconds.
    pub timeout: Option<u64>,
    /// Destination allow-list. Empty means any host.
    pub allowed_subnets: AllowedNets,
}

/// Opens a NETCONF session for one tool call (or for `subscribe`).
///
/// The CLI injects SSH-config / env / agent. Embedders bring their own stack.
///
/// When [`ConnectParams::allowed_subnets`] is non-empty, apply any name rewrite
/// (`HostName`, aliases) first, then [`AllowedNets::pin`] the **device**
/// address and dial that. Do not re-resolve the original hostname after the
/// check. Jump hosts are the path, not the target; they are not filtered here.
#[async_trait::async_trait]
pub trait DeviceConnect: Send + Sync + 'static {
    /// Dial, authenticate, and complete `<hello>`.
    async fn connect(&self, params: ConnectParams) -> NetconfClientResult<Connection>;
}
