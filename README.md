# netconf-rust

Async [NETCONF](https://www.rfc-editor.org/rfc/rfc6241.html) client library and CLI, written in Rust.

[![CI](https://img.shields.io/github/actions/workflow/status/Mrflatt/netconf-rust/ci.yaml?style=flat-square)](https://github.com/Mrflatt/netconf-rust/actions/workflows/ci.yaml)
[![crates.io](https://img.shields.io/crates/v/netconf-async.svg?style=flat-square)](https://crates.io/crates/netconf-async)
[![docs.rs](https://img.shields.io/docsrs/netconf-async?style=flat-square)](https://docs.rs/netconf-async)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

Talk to routers and switches over SSH using the `netconf` subsystem. The library handles hello exchange, 1.0/1.1 framing, and typed RPCs. The CLI fans the same operations out to one or more hosts, honoring `~/.ssh/config`.

## Features

- Async client on Tokio (`netconf-async`)
- SSH transport via the NETCONF subsystem ([RFC 6242](https://www.rfc-editor.org/rfc/rfc6242.html))
- Automatic upgrade from base 1.0 (`]]>]]>`) to base 1.1 chunked framing
- RPCs: `<get>`, `<get-config>`, `<edit-config>`, `<copy-config>`, `<delete-config>`, `<lock>` / `<unlock>`, `<validate>`, `<commit>` / confirmed-commit / `<cancel-commit>`, `<discard-changes>`, `<close-session>`, `<kill-session>`, raw RPC
- Event notifications via `<create-subscription>` ([RFC 5277](https://www.rfc-editor.org/rfc/rfc5277.html))
- Subtree filters and `with-defaults` ([RFC 6243](https://www.rfc-editor.org/rfc/rfc6243.html))
- CLI with parallel multi-host runs, OpenSSH config, ssh-agent, `ProxyJump`, inventory CSV, and XML templates

## Crates

| Crate | What |
|---|---|
| [`netconf-async`](netconf-async) | Library: `Connection`, messages, framer, SSH transport |
| [`netconf-cli`](netconf-cli) | CLI binary `netconf-cli` (help text says `netconf`) |

Rust edition **2024** (rustc 1.85+). Workspace resolver 3.

## Installation

Library from [crates.io](https://crates.io/crates/netconf-async):

```toml
[dependencies]
netconf-async = "0.1"
```

CLI from source:

```bash
git clone https://github.com/Mrflatt/netconf-rust.git
cd netconf-rust
cargo install --path netconf-cli
# or without installing:
cargo run -p netconf-cli -- --help
```

Or download a release archive for your target and extract `netconf-cli`:

```text
netconf-cli-{version}-{target}.tar.gz
```

Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

```bash
netconf-cli update          # install latest GitHub release
netconf-cli update --check  # report only
```

## Library

```rust
use netconf_async::connection::Connection;
use netconf_async::message::{Datastore, Filter};
use netconf_async::transport::ssh::{HostKeyPolicy, SshAuth, SshConfig, SSHTransport};

#[tokio::main]
async fn main() -> netconf_async::error::NetconfClientResult<()> {
    let transport = SSHTransport::connect(
        SshConfig::new("192.0.2.10", 830, "netconf", SshAuth::password("secret"))
            .host_key(HostKeyPolicy::Fingerprint("SHA256:base64fingerprint".into())),
    ).await?;
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

`Connection::new` performs the `<hello>` exchange and upgrades the framer when the server advertises `urn:ietf:params:netconf:base:1.1`. Call `close_session` when you are done; dropping an open session only logs a warning, because a clean shutdown has to await I/O.

Other methods on `Connection`: `edit_config`, `copy_config`, `delete_config`, `lock`, `unlock`, `validate`, `commit`, `confirmed_commit`, `confirm_commit`, `cancel_commit`, `discard_changes`, `kill_session`, `raw_rpc`, `notification`. `has_capability` reflects the server `<hello>`.

### Features

| Feature | Default | Purpose |
|---|---|---|
| `ssh` | yes | `SSHTransport` over `russh` (implies `tokio`) |
| `tokio` | yes | Tokio framer, RPC timeouts, notification streams |

## CLI

```bash
# running config (logs on stderr, reply on stdout)
netconf-cli get-config --host router.example --username netconf > running.xml

# candidate + subtree filter + with-defaults
netconf-cli get-config \
  --host 192.0.2.10 \
  --username netconf \
  --source candidate \
  --filter interfaces.xml \
  --with-defaults trim

# operational state (filter file required)
netconf-cli get --host router.example --username netconf -f system.xml

# edit: lock → edit-config → validate → commit → unlock → copy running→startup
netconf-cli edit --host router.example --username netconf -f changes.xml
# directory: apply 1-foo.xml, 2-bar.xml in name order, then commit once
netconf-cli edit --host router.example --username netconf -f ./changes/

# persist confirmed-commit, then confirm (or --cancel) from another session
netconf-cli edit --host router.example --username netconf -f changes.xml --confirmed
netconf-cli commit --host router.example --username netconf --id <persist-id>

# raw RPC document (or a directory of them, executed in name order)
netconf-cli rpc --host router.example --username netconf -f custom-rpc.xml

# notifications (Ctrl-C to stop)
netconf-cli notification --host router.example --username netconf
netconf-cli notification --host router.example --username netconf --get   # list streams

# several devices in parallel (one file per host)
netconf-cli get-config --host r1.example,r2.example --output-dir ./configs
netconf-cli get-config --host r1 --host r2 --output-dir ./configs

# inventory CSV (@file) + Go-template subset, preview without SSH
netconf-cli edit --host @hosts.csv --file sap.xml --dry-run
netconf-cli edit --host @hosts.csv --file sap.xml --dry-run --output-dir ./rendered

# pretty XML or JSON
netconf-cli get-config --host router.example --format pretty
netconf-cli get-config --host router.example --format json,pretty > running.json

# self-update from GitHub Releases
netconf-cli update
netconf-cli update --check
```

From the workspace without installing, prefix the same arguments with `cargo run -p netconf-cli --`.

### Auth and SSH config

Resolution order for user: `--username` / `NETCONF_USERNAME`, then `User` from `~/.ssh/config`. Password is `--password` / `NETCONF_PASSWORD`. If no password is given, the first `IdentityFile` is used, otherwise the CLI walks identities from `ssh-agent`.

`HostName`, `Port`, `User`, `IdentityFile`, `Compression`, `TCPKeepAlive` + `ServerAliveInterval`, algorithm prefs (`Ciphers`, `KexAlgorithms`, `MACs`, `HostKeyAlgorithms`), `UserKnownHostsFile`, and `ProxyJump` (any number of hops) are read from `~/.ssh/config`. Each jump authenticates with its own `IdentityFile` or the agent — the device password is not reused. Host keys default to `accept-new`: unknown hosts are pinned into `UserKnownHostsFile` or `~/.ssh/known_hosts`, a later key change is rejected. `--strict-host-key-checking yes` rejects unknown hosts too. `--strict-host-key-checking no` accepts any key.

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
| `NETCONF_RELEASE_REPO` | GitHub `owner/repo` for `update` (default `Mrflatt/netconf-rust`) |
| `NETCONF_GITHUB_TOKEN` / `GH_TOKEN` | Optional token for GitHub API rate limits |

### Output

Replies go to **stdout**. Logs go to **stderr**, so `> reply.xml` is just the document.

| Flag | Effect |
|---|---|
| `--format xml` | raw reply (default) |
| `--format pretty` | indented XML |
| `--format pretty,unescape` | decode entities, then pretty-print |
| `--format json` | XML → JSON |
| `--format json,pretty` | indented JSON |
| `--output-dir DIR` | write `DIR/{host}.xml` (or `.json`) instead of stdout |

`--format` is comma-separated tokens: one codec (`xml` or `json`) plus `pretty` and/or `unescape`. More codecs can be added later without a new flag.

### Inventory and templates

`--host` takes a name, a comma-separated list, or `@file.csv`. Repeat the flag to mix them. `--delimiter` changes the CSV separator (default `,`).

A header with `ip` or `host` turns extra columns into template variables. The same address on many rows becomes a slice; the template must `range`. A bare IP list (no header) is just hosts.

`--file` / `--config` XML then understands a Go-template subset: `{{ .field }}`, `{{ env "NAME" }}`, `{{ range . }}` … `{{ end }}`, plus `{{-` / `-}}` trim. `--template` forces that parse when the inventory has no extra columns.

`--dry-run` / `--dry-run=data` prints the rendered XML and does not connect. A later `session` mode may hello and print RPCs; it is not implemented yet.

### Logging

| Flag | Effect |
|---|---|
| _(none)_ | info on stderr, library quiet |
| `-v` | debug, library quiet (no russh / ssh2_config) |
| `-vv` | debug + connection + russh / ssh2_config |
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
