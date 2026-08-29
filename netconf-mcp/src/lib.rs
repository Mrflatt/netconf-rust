#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

//! MCP server for NETCONF. The product entry is `netconf-cli mcp`.

#[cfg(not(any(feature = "stdio", feature = "http")))]
compile_error!("enable feature `stdio` and/or `http`");

mod config;
mod connect;
mod edit;
mod host;
mod notification;
mod server;
mod types;

pub use async_trait::async_trait;
pub use config::{McpConfig, McpTransport};
pub use connect::{ConnectParams, DeviceConnect};
pub use host::AllowedNets;

use thiserror::Error;

/// Failure starting or running the MCP server.
#[derive(Debug, Error)]
pub enum McpServeError {
    /// Bind or accept failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// rmcp service failed.
    #[error("{0}")]
    Serve(String),
}

/// Run the MCP server until the client disconnects or the process is signalled.
pub async fn serve<C: DeviceConnect>(config: McpConfig, connector: C) -> Result<(), McpServeError> {
    server::run(config, connector).await
}

pub(crate) fn mcp_err(err: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(err.to_string(), None)
}

pub(crate) fn mcp_params(err: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(err.to_string(), None)
}
