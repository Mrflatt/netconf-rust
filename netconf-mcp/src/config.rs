use crate::host::AllowedNets;

/// How the MCP server talks to the client.
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// stdin / stdout (Claude Desktop, Cursor, pi).
    #[cfg(feature = "stdio")]
    Stdio,
    /// Streamable HTTP. Bind must be loopback (`127.0.0.1` / `::1`).
    #[cfg(feature = "http")]
    Http {
        /// Listen address. Non-loopback is rejected at serve time.
        bind: std::net::SocketAddr,
    },
}

/// Server knobs. Built by `netconf-cli mcp` or an embedder.
#[derive(Debug, Clone)]
pub struct McpConfig {
    /// Stdio or HTTP.
    pub transport: McpTransport,
    /// Hide write tools (`rpc`, `edit_config`, `copy_config`, `commit`).
    pub read_only: bool,
    /// If non-empty, tool `host` must resolve inside one of these CIDRs.
    pub allowed_subnets: AllowedNets,
    /// Advertised server version.
    pub version: String,
}

impl McpConfig {
    /// Stdio, writable, no subnet filter.
    #[cfg(feature = "stdio")]
    pub fn stdio() -> Self {
        Self {
            transport: McpTransport::Stdio,
            read_only: false,
            allowed_subnets: AllowedNets::allow_all(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// HTTP on `127.0.0.1:port`.
    #[cfg(feature = "http")]
    pub fn http_localhost(port: u16) -> Self {
        Self {
            transport: McpTransport::Http {
                bind: std::net::SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    port,
                ),
            },
            read_only: false,
            allowed_subnets: AllowedNets::allow_all(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Parse CIDR strings. Empty means allow every host.
    pub fn with_allowed_subnets(
        mut self,
        cidrs: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, String> {
        self.allowed_subnets = AllowedNets::parse(cidrs)?;
        Ok(self)
    }
}
