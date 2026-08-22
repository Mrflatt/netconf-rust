# AGENTS.md

Context for coding agents working in this repo.

## Commands

Workspace root is the Cargo workspace. Always run from here.

```bash
cargo fmt --all                     # format
cargo fmt --all --check             # CI rustfmt
cargo clippy --workspace --all-targets --no-deps -- -D warnings
cargo test                          # all crates
cargo test -p netconf-async         # library only
cargo test -p netconf-cli           # CLI only
cargo test -p netconf-async test_serialize_get_config   # one test
cargo test -p netconf-cli parse_jump_spec_variants
cargo build --workspace
cargo update -p netconf-async --locked   # CI lockfile check
cargo update -p netconf-cli --locked
cargo run -p netconf-cli -- --help
```

Windows (matches CI):

```bash
cargo test --features openssl-on-win32,vendored-openssl,default
```

Toolchain is `stable` from `rust-toolchain.toml` (includes `rustfmt`, `clippy`). No `rustfmt.toml` / `clippy.toml` — rustfmt defaults, clippy `-D warnings`.

## Testing

Tests live next to the code they cover, under `#[cfg(test)]`. There is no `tests/` directory.

| Crate | File | What |
|---|---|---|
| `netconf-async` | `src/message.rs` | Hello / RPC serialize, `RpcReply` deserialize |
| `netconf-async` | `src/framer/async_framer.rs` | 1.0 EOM + 1.1 chunked framing (`#[tokio::test]`) |
| `netconf-cli` | `src/cli.rs` | `cli().debug_assert()` |
| `netconf-cli` | `src/config.rs` | ProxyJump / host:port / ssh_config parsing |

- Add a test for every new RPC variant, framer edge, or host-string parse.
- XML tests pin a fixed `message-id` and compare the full document with `pretty_assertions::assert_eq`.
- Do not add live-device tests. CI sets `CARGO_PUBLIC_NETWORK_TESTS=1` but nothing reads it.
- A failing test is a bug. Fix it; never delete or `#[ignore]` it to go green.

Single-test filter is the function name:

```bash
cargo test -p netconf-async test_deserialize_rpc_reply -- --nocapture
```

## Project structure

```
.
├── Cargo.toml                 # workspace, resolver = "3"
├── rust-toolchain.toml        # stable + rustfmt + clippy
├── netconf-async/             # library crate
│   └── src/
│       ├── lib.rs             # re-exports + NETCONF URN constants
│       ├── connection.rs      # session: hello, RPCs, notifications, Drop
│       ├── message.rs         # Hello / Rpc / RpcReply / Filter / Datastore
│       ├── error.rs           # NetconfClientError + NetconfClientResult
│       ├── framer.rs          # Framer trait, ]]>]]> terminator
│       ├── framer/async_framer.rs
│       ├── transport.rs       # Transport trait
│       ├── transport/ssh.rs   # SSH netconf subsystem
│       └── transport/tls.rs   # empty stub — not a module, do not import
└── netconf-cli/               # CLI crate, binary name netconf-cli
    └── src/
        ├── main.rs
        ├── cli.rs             # clap root + parallel host fan-out
        ├── config.rs          # ~/.ssh/config, ProxyJump, Host
        └── commands/
            ├── builtin.rs     # dispatch + filter file helper
            ├── get.rs
            ├── get_config.rs
            └── notification.rs
```

Implemented `Connection` RPCs: `get`, `get-config`, `validate`, `commit`, `confirmed_commit`, `close_session`, `kill_session`, `notification` (`<create-subscription>`, RFC 5277).

Implemented CLI subcommands: `get`, `get-config`, `notification`. The help template also lists `edit`, `copy`, `rpc` — those are **not** implemented. Do not document them as working. Do not add them unless asked.

## Stack

- Rust **edition 2024**, workspace resolver **3**. Needs rustc **1.85+**. No `rust-version` pin; CI uses current stable.
- Tokio 1 (multi-thread). Traits use `async-trait`.
- XML via `quick-xml` + `serde` / `serde_derive`.
- SSH: `async-ssh2-lite` (tokio) in the library; CLI also uses `ssh2` + `ssh2-config`.
- Errors: `thiserror` → `NetconfClientError`, alias `NetconfClientResult<T>`.
- CLI: clap 4 builder API (not derive on command structs), `env_logger`, `color-print`.

Library features (`netconf-async`):

| Feature | Default | Notes |
|---|---|---|
| `tokio` | yes | Connection Drop, notifications |
| `async-ssh2-lite` | yes | `transport::ssh` |
| `async-trait` | yes | `Transport` / `Framer` |
| `vendored-openssl` | no | forwarded to `async-ssh2-lite` |
| `openssl-on-win32` | no | Windows CI |

`lib.rs` crate docs still say `netconf-async = "^0.2.0"`. Package version is **0.1.0**. Do not copy the `0.2.0` line.

## Protocol notes

Session flow:

1. SSH connect, request subsystem `netconf`.
2. Exchange `<hello>` with **1.0** framing (`]]>]]>`).
3. If the server advertises `urn:ietf:params:netconf:base:1.1`, call `transport.upgrade()` → chunked framing (`\n#N\n...\n##\n`, RFC 6242).
4. Each RPC is `write_and_receive`. Reply is parsed as `RpcReply`; any `<rpc-error>` becomes `NetconfClientError::Netconf`.
5. `close_session` sets `is_closed`. `Drop` (tokio feature) also closes unless `is_closed`.

`Connection::set_skip_serializing` skips reply parse and returns raw XML.

Default NETCONF-over-SSH port is **830**. Jump hosts use **22**.

## Code style

Match the file you are in. Library prefers `core::` for `fmt` / `str` / `time`; CLI uses `std::`.

```rust
// Good — typed RPC, Result alias, RFC-shaped API
pub async fn get_config(
    &mut self,
    datastore: Datastore,
    filter: Option<Filter>,
    defaults: Option<WithDefaultsValue>,
) -> NetconfClientResult<String> {
    let rpc = Rpc::new_with_operation(RpcOperation::new_get_config(datastore, filter, defaults));
    self.run_rpc(rpc).await
}

// Bad
pub async fn get_config(&mut self, ds: &str, f: Option<String>) -> Result<String, Box<dyn Error>> {
    unimplemented!()
}
```

- Types `PascalCase`, functions `snake_case`. Serde renames stay kebab-case (`get-config`, `session-id`).
- New RPC: variant on `RpcOperation` → constructor → `Connection` method → serialize test with a fixed message-id → CLI only if requested.
- Errors go through `NetconfClientError` / `NetconfClientError::new`. Do not introduce `anyhow::Result` in the library public API (CLI may use `anyhow` internally via the error variant).
- Comments: what and why, plus RFC section links. No changelog comments.
- Exported types and methods get rustdoc. Follow existing RFC links (`RFC6241`, `RFC6242`, `RFC5277`).
- `#![allow(dead_code)]` on `message.rs` is existing. Do not spread it.
- Edition 2024 let-chains are already used (`if cond && let Some(x) = ...`). Fine to use.

## Boundaries

- Never commit secrets, device passwords, or `*.local.sh`. CLI password flag is `--password` / `NETCONF_PASSWORD`.
- Never edit `target/`. Never hand-edit `Cargo.lock` except via `cargo` when changing deps.
- Never disable rustfmt, clippy, or hooks. Never add `#[allow]` to silence a real warning you introduced.
- `netconf-async/src/transport/tls.rs` is a zero-byte stub and is **not** `mod`ed. Do not `pub mod tls` or pretend TLS works.
- Do not implement `edit` / `copy` / `rpc` CLI commands, TLS transport, or multi-hop ProxyJump unless asked. CLI ProxyJump supports **one** hop; jump auth is ssh-agent only (device password is not reused).
- Do not bump crate versions or publish.
- Do not rewrite working serialize/framer tests to “simplify” them.
- Filter files are subtree XML. `Filter::subtree` unescapes `\"` sequences; keep that behavior.
