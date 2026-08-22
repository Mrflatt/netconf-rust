# netconf-rust

Async [NETCONF](https://www.rfc-editor.org/rfc/rfc6241.html) client library and CLI, written in Rust.

[![CI](https://img.shields.io/github/actions/workflow/status/Mrflatt/netconf-rust/ci.yaml?style=flat-square)](https://github.com/Mrflatt/netconf-rust/actions/workflows/ci.yaml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

Talk to routers and switches over SSH using the `netconf` subsystem. The library handles hello exchange, 1.0/1.1 framing, and typed RPCs. The CLI fans the same operations out to one or more hosts, honoring `~/.ssh/config`.

## Features

- Async client on Tokio (`netconf-async`)
- SSH transport via the NETCONF subsystem ([RFC 6242](https://www.rfc-editor.org/rfc/rfc6242.html))
- Automatic upgrade from base 1.0 (`]]>]]>`) to base 1.1 chunked framing
- RPCs: `<get>`, `<get-config>`, `<validate>`, `<commit>` / confirmed-commit, `<close-session>`, `<kill-session>`
- Event notifications via `<create-subscription>` ([RFC 5277](https://www.rfc-editor.org/rfc/rfc5277.html))
- Subtree filters and `with-defaults` ([RFC 6243](https://www.rfc-editor.org/rfc/rfc6243.html))
- CLI with parallel multi-host runs, OpenSSH config, ssh-agent, and single-hop `ProxyJump`

## Crates

| Crate | What |
|---|---|
| [`netconf-async`](netconf-async) | Library: `Connection`, messages, framer, SSH transport |
| [`netconf-cli`](netconf-cli) | CLI binary `netconf-cli` (help text says `netconf`) |

Rust edition **2024** (rustc 1.85+). Workspace resolver 3.

## Installation

The crates are not published to crates.io yet. Use the git workspace:

```toml
[dependencies]
netconf-async = { git = "https://github.com/Mrflatt/netconf-rust" }
```

CLI:

```bash
git clone https://github.com/Mrflatt/netconf-rust.git
cd netconf-rust
cargo install --path netconf-cli
# or without installing:
cargo run -p netconf-cli -- --help
```

## Library

```rust
use netconf_async::connection::Connection;
use netconf_async::message::{Datastore, Filter};
use netconf_async::transport::ssh::SSHTransport;

#[tokio::main]
async fn main() -> netconf_async::error::NetconfClientResult<()> {
    let transport =
        SSHTransport::new_with_user_auth("192.0.2.10:830", "netconf", "secret").await?;
    let mut conn = Connection::new(transport).await?;

    let running = conn.get_config(Datastore::Running, None, None).await?;
    println!("{running}");

    let filter = Filter::subtree(
        r#"<system xmlns="urn:ietf:params:xml:ns:yang:ietf-system"/>"#,
    );
    let filtered = conn.get(Some(filter), None).await?;
    println!("{filtered}");

    conn.close_session().await?;
    Ok(())
}
```

`Connection::new` performs the `<hello>` exchange and upgrades the framer when the server advertises `urn:ietf:params:netconf:base:1.1`. If you skip `close_session`, `Drop` will try to close the session.

Other methods on `Connection`: `validate`, `commit`, `confirmed_commit`, `kill_session`, `notification`.

### Features

| Feature | Default | Purpose |
|---|---|---|
| `tokio` | yes | runtime integration, notifications, `Drop` |
| `async-ssh2-lite` | yes | `SSHTransport` |
| `async-trait` | yes | `Transport` / `Framer` traits |
| `vendored-openssl` | no | static OpenSSL (also used on Windows CI) |
| `openssl-on-win32` | no | Windows OpenSSL |

## CLI

```bash
# running config
netconf-cli get-config --host router.example --username netconf

# candidate + subtree filter + with-defaults
netconf-cli get-config \
  --host 192.0.2.10 \
  --username netconf \
  --source candidate \
  --filter interfaces.xml \
  --with-defaults trim

# operational state (filter file required)
netconf-cli get --host router.example --username netconf -f system.xml

# notifications (Ctrl-C to stop)
netconf-cli notification --host router.example --username netconf
netconf-cli notification --host router.example --username netconf --get   # list streams

# several devices in parallel
netconf-cli get-config --host r1.example,r2.example --username netconf
```

From the workspace without installing, prefix the same arguments with `cargo run -p netconf-cli --`.

### Auth and SSH config

Resolution order for user: `--username` / `NETCONF_USERNAME`, then `User` from `~/.ssh/config`. Password is `--password` / `NETCONF_PASSWORD`. If no password is given, the CLI walks identities from `ssh-agent`.

`HostName`, `Port`, `User`, algorithms, compression, keepalives, and a **single** `ProxyJump` hop are read from `~/.ssh/config`. The jump host authenticates with the agent only — the device password is not reused.

```sshconfig
Host jump
  HostName jump.example
  User jump-user
  Port 22

Host router
  HostName 192.0.2.10
  User netconf
  Port 830
  ProxyJump jump
```

```bash
netconf-cli get-config --host router
```

> [!NOTE]
> Default NETCONF-over-SSH port is **830**, not 22. An explicit `:port` on `--host` wins over `Port` in ssh_config. IPv6 with a port must be written `[2001:db8::10]:830`.

### Environment

| Variable | Meaning |
|---|---|
| `NETCONF_HOST` | Host list (comma-separated, same as `--host`) |
| `NETCONF_USERNAME` | Username |
| `NETCONF_PASSWORD` | Password (hidden in help) |
| `NETCONF_WITH_DEFAULTS` | `report-all`, `report-all-tagged`, `trim`, `explicit` |

### Logging

| Flag | Effect |
|---|---|
| _(none)_ | info, library quiet |
| `-v` | debug, library quiet |
| `-vv` | debug + connection |
| `-vvv` | debug including RPC frames |
| `-q` | no logs |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --no-deps -- -D warnings
cargo test
```

CI runs rustfmt, clippy (`-D warnings`), a lockfile check, and `cargo test` on Linux, macOS, and Windows.

See [AGENTS.md](AGENTS.md) for crate layout, protocol flow, and how to add an RPC.
