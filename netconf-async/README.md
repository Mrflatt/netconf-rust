# netconf-async

Async [NETCONF](https://www.rfc-editor.org/rfc/rfc6241.html) client for Rust.

[![crates.io](https://img.shields.io/crates/v/netconf-async.svg)](https://crates.io/crates/netconf-async)
[![docs.rs](https://docs.rs/netconf-async/badge.svg)](https://docs.rs/netconf-async)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Talk to routers and switches over SSH using the `netconf` subsystem. The crate
handles the `<hello>` exchange, base 1.0 / 1.1 framing, and typed RPCs.

## Install

```toml
[dependencies]
netconf-async = "0.1"
```

Requires rustc **1.85+** (edition 2024).

Default features pull in Tokio and SSH (`russh`, pure Rust).

## Example

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

`Connection::new` performs the `<hello>` exchange and upgrades the framer when
the server advertises `urn:ietf:params:netconf:base:1.1`. Call `close_session`
when you are done: a clean shutdown has to await I/O, which `Drop` cannot do, so
dropping an open session only logs a warning.

`SSHTransport::connect` takes `SshAuth` (password, agent, or key file) and an
optional ProxyJump chain. Host-key policy defaults to `RejectAll`; pin a
fingerprint with `HostKeyPolicy::Fingerprint` or an OpenSSH file with
`HostKeyPolicy::KnownHosts` / `HostKeyPolicy::AcceptNew`.

A custom byte pipe implements [`Transport`](https://docs.rs/netconf-async/latest/netconf_async/transport/trait.Transport.html)
and is passed to `Connection::new` the same way.

## Session flow

1. SSH connect, request subsystem `netconf`.
2. Exchange `<hello>` with **1.0** framing (`]]>]]>`).
3. If the server advertises `:base:1.1`, call `transport.upgrade()` → chunked
   framing (`\n#N\n...\n##\n`, [RFC 6242](https://www.rfc-editor.org/rfc/rfc6242.html)).
4. Each RPC is `write_and_receive`. The reply is parsed as `RpcReply`; the
   `message-id` must match. Error-severity `<rpc-error>` becomes
   `NetconfClientError::Netconf`. Warning-only replies succeed unless
   `set_warnings_as_errors(true)`. EOF after `<commit>` is `CommitUnknown`.
5. `close_session` ends the session and tears the transport down.

`Connection::set_parse_replies(false)` returns the device's XML untouched and
stops `<rpc-error>` from becoming an error. `Connection::set_timeout` bounds a
single RPC; a timed-out session is marked unusable, because a late reply would
be read as the answer to the next request.

Default NETCONF-over-SSH port is **830**.

## RPCs

| Method | Operation |
|---|---|
| `get` | `<get>` |
| `get_config` | `<get-config>` |
| `edit_config` | `<edit-config>` |
| `copy_config` | `<copy-config>` |
| `delete_config` | `<delete-config>` |
| `lock` / `unlock` | `<lock>` / `<unlock>` |
| `validate` | `<validate>` |
| `commit` / `confirmed_commit` / `confirm_commit` / `cancel_commit` | `<commit>` family |
| `discard_changes` | `<discard-changes>` |
| `close_session` / `kill_session` | session control |
| `raw_rpc` | caller-supplied XML |
| `notification` | `<create-subscription>` ([RFC 5277](https://www.rfc-editor.org/rfc/rfc5277.html)) |

Subtree filters and `with-defaults` ([RFC 6243](https://www.rfc-editor.org/rfc/rfc6243.html))
are supported on `get` / `get-config`. `has_capability` reflects the server
`<hello>` (query string ignored).

Filter and `<config>` payloads reach the device byte for byte; everything the
crate generates around them stays XML-escaped. `RpcReply::errors()` exposes each
`<rpc-error>`, including vendor tags outside RFC 6241.

## Crate features

| Feature | Default | Purpose |
|---|---|---|
| `ssh` | yes | `SSHTransport` over `russh` (implies `tokio`) |
| `tokio` | yes | Tokio framer, RPC timeouts, notification streams |

## Related

A CLI that fans these operations out to one or more hosts lives in the same
repository: [netconf-cli](https://github.com/Mrflatt/netconf-rust).
