# netconf-mcp

MCP server for [NETCONF](https://www.rfc-editor.org/rfc/rfc6241.html) over SSH.

This crate is a library. The product entry is `netconf-cli mcp`. Embedders call
[`serve`] with their own [`DeviceConnect`] implementation.

Requires rustc **1.88+**.

## Install

```toml
[dependencies]
netconf-mcp = "0.1"
```

Default features serve stdio. Enable `http` for streamable HTTP.
`--no-default-features --features http` is HTTP-only.

## Example

```rust,no_run
use netconf_mcp::{DeviceConnect, McpConfig, serve};

#[tokio::main]
async fn main() -> Result<(), netconf_mcp::McpServeError> {
    serve(McpConfig::stdio(), MyConnector).await
}

struct MyConnector;

#[netconf_mcp::async_trait]
impl DeviceConnect for MyConnector {
    async fn connect(
        &self,
        params: netconf_mcp::ConnectParams,
    ) -> netconf_async::NetconfClientResult<netconf_async::Connection> {
        let _ = params;
        unimplemented!("open SSH + Connection::new")
    }
}
```

## Tools

| Tool | Role |
|---|---|
| `get` | operational `<get>` (`output_file` writes large replies to disk) |
| `get_config` | `<get-config>` (`output_file` writes large replies to disk) |
| `rpc` | raw RPC (no `<rpc>` wrapper required; `output_file` optional) |
| `edit_config` | lock → edit → validate → commit → unlock |
| `copy_config` | datastore or URL |
| `commit` | confirm / cancel a persist-id |
| `list_schemas` | ietf-netconf-monitoring schemas |
| `get_schema` | `<get-schema>` (`output_file` optional) |
| `subscribe` | hold a listen-only notification session |
| `notification_pull` | drain buffered notifications |
| `notification_cancel` | close a subscription |

`--read-only` hides `rpc`, `edit_config`, `copy_config`, and `commit`.

`--http` listens on loopback only. `--allowed-subnet` pins the device address
after SSH `HostName` expansion so a later DNS lookup cannot leave the list.

Tool `host` may include a port (`192.0.2.1:830`, `[2001:db8::1]:830`).
