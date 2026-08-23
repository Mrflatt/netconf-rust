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

SSH is `russh` (pure Rust). No OpenSSL.

Toolchain is `stable` from `rust-toolchain.toml` (includes `rustfmt`, `clippy`). No `rustfmt.toml` / `clippy.toml` — rustfmt defaults, clippy `-D warnings`.

## Testing

Unit tests live next to the code they cover, under `#[cfg(test)]`. Session-level tests against a scripted in-memory device live in `netconf-async/tests/session.rs`.

| Crate | File | What |
|---|---|---|
| `netconf-async` | `src/message.rs` | Hello / RPC serialize, `RpcReply` deserialize |
| `netconf-async` | `src/connection.rs` | `message-id` extract, commit-RPC detect |
| `netconf-async` | `src/transport/ssh.rs` | `SshConfig` builder, host-key policy |
| `netconf-async` | `src/framer/async_framer.rs` | 1.0 EOM + 1.1 chunked framing (`#[tokio::test]`) |
| `netconf-async` | `tests/session.rs` | scripted device: hello, RPC, timeout, warnings, commit EOF |
| `netconf-cli` | `src/cli.rs` | `cli().debug_assert()` |
| `netconf-cli` | `src/config.rs` | ProxyJump / host:port / ssh_config parsing |
| `netconf-cli` | `src/commands/builtin.rs` | XML file/dir load (`1-rpc.xml` name order) |
| `netconf-cli` | `src/output.rs` | `--format` parse, XML pretty/unescape, XML→JSON, `--output-dir` |
| `netconf-cli` | `src/inventory.rs` | `--host @file.csv`, delimiter, duplicate-IP var slices |
| `netconf-cli` | `src/template.rs` | Go-template subset: field, `env`, `range`, trim |
| `netconf-cli` | `src/update.rs` | tag parse, asset match, GitHub digest, archive extract, poller |

- Add a test for every new RPC variant, framer edge, host-string parse, or XML file/dir loader.
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
├── .github/workflows/
│   ├── ci.yaml                # rustfmt, clippy, lockfile, test, release-please
│   └── release-binaries.yaml  # build + upload netconf-cli archives to the GitHub release
├── netconf-async/             # library crate
│   └── src/
│       ├── lib.rs             # re-exports + NETCONF URN constants
│       ├── connection.rs      # session: hello, RPCs, timeouts, notifications
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
        ├── inventory.rs       # --host @file.csv → targets + vars
        ├── output.rs          # --format / --output-dir reply emit
        ├── template.rs        # Go-template subset for --file XML
        ├── update.rs          # GitHub release poller + self-replace
        └── commands/
            ├── builtin.rs     # dispatch + filter/xml file-or-dir helper
            ├── get.rs
            ├── get_config.rs
            ├── edit.rs
            ├── copy.rs
            ├── commit.rs
            ├── rpc.rs
            ├── notification.rs
            └── update.rs      # update subcommand (no device session)
```

Implemented `Connection` RPCs: `get`, `get-config`, `edit_config`, `copy_config`, `delete_config`, `lock`, `unlock`, `validate`, `commit`, `confirmed_commit`, `confirm_commit`, `cancel_commit`, `discard_changes`, `close_session`, `kill_session`, `raw_rpc`, `notification` (`<create-subscription>`, RFC 5277).

Implemented CLI subcommands: `get`, `get-config`, `edit`, `copy`, `commit`, `rpc`, `notification`, `update`.
`edit` orchestrates lock → edit-config (all `--file` XML, name order) → validate → commit once → unlock → optional running→startup copy. `rpc` executes each `--file` XML in name order. `commit` confirms or cancels a persist confirmed-commit. `update` polls GitHub releases and does not open a device session.

## Stack

- Rust **edition 2024**, workspace resolver **3**. `rust-version = "1.85"` on `netconf-async`; CI uses current stable.
- Tokio 1 (multi-thread). Traits use `async-trait`.
- XML via `quick-xml` + `serde` (derive feature). Substring search via `memchr`.
- SSH: `russh` (tokio) inside the library only. CLI parses `~/.ssh/config` with `ssh2-config` and calls `SSHTransport::connect`.
- Errors: `thiserror` → `NetconfClientError`, alias `NetconfClientResult<T>`.
- CLI: clap 4 builder API (not derive on command structs), `env_logger`, `color-print`.

Library features (`netconf-async`):

| Feature | Default | Notes |
|---|---|---|
| `ssh` | yes | `transport::ssh` via `russh`; implies `tokio` |
| `tokio` | yes | `AsyncFramer`, RPC timeouts, notifications |

`async-trait` is a plain dependency: the `Transport` and `Framer` traits need it
unconditionally, so it must not be optional. Every feature combination has to
build — check with `cargo check --no-default-features --features ...`.

Crate-level rustdoc lives in `netconf-async/src/lib.rs`; crates.io renders `netconf-async/README.md`. Do not hardcode a crate version that does not match `Cargo.toml`.

## Protocol notes

Session flow:

1. SSH connect, request subsystem `netconf`.
2. Exchange `<hello>` with **1.0** framing (`]]>]]>`).
3. If the server advertises `urn:ietf:params:netconf:base:1.1`, call `transport.upgrade()` → chunked framing (`\n#N\n...\n##\n`, RFC 6242).
4. Each RPC is `write_and_receive`. Reply is parsed as `RpcReply`; `message-id` must match (mismatch desynchronizes). Error-severity `<rpc-error>` becomes `NetconfClientError::Netconf`. Warning-only is success unless `set_warnings_as_errors(true)`. EOF after `<commit>` / `<commit-configuration>` is `CommitUnknown` — thread `is_commit` through the send, never a session flag.
5. `close_session` sends the RPC and then closes the transport. `Drop` cannot await I/O, so it only warns when the session is still open.

`SSHTransport::connect(SshConfig)` owns TCP, auth (`SshAuth::{Password,Agent,KeyFile}`), ProxyJump chain (`SshConfig::jump` appends hops), host-key policy (`RejectAll` default, `Fingerprint`, `KnownHosts`, `AcceptAll` lab opt-in), and session knobs (`SshSessionOpts`: compression, keepalive, kex/cipher/mac prefs). Each hop is a russh `direct-tcpip` channel used as the next handshake stream. Do not leak `russh` types (`NetconfClientError::Ssh` is a `String`). CLI maps `IdentityFile`. Host keys default to `accept-new` (pin unknown, reject changed). `--strict-host-key-checking` / `StrictHostKeyChecking`: `yes`, `accept-new`, `no`.

`Connection::set_parse_replies(false)` skips the reply parse and returns raw XML.
`Connection::set_timeout` bounds one RPC; on timeout the session is marked
desynchronized, because a late reply would be read as the next answer.

Replies are parsed through `message::from_xml`, which retries with namespace
prefixes stripped so `<nc:hello>` / `<nc:rpc-reply>` devices still work.
`ErrorTag` / `ErrorType` / `ErrorSeverity` keep unknown values in an `Other`
variant rather than failing the whole parse.

Default NETCONF-over-SSH port is **830**. Jump hosts use **22**.

## Releases

Repo has immutable GitHub releases: a published release cannot gain or change assets. release-please therefore creates `netconf-cli-v*` as a **draft** (`draft` + `force-tag-creation` so the tag exists immediately). CI builds archives, uploads them to the draft, then publishes. After that the release is frozen. `netconf-async` has no binaries and is published immediately.

`GITHUB_TOKEN` cannot trigger a second workflow, so binaries run in the same CI run. workflow_dispatch can finish a leftover draft; it cannot patch an already-published release.

Assets: `netconf-cli-{version}-{target}.tar.gz`.

`netconf update` polls `Mrflatt/netconf-rust` releases, ignores `netconf-async-*` tags, verifies GitHub's asset `digest`, then `self_replace`. It does **not** open a device session.

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
- There is no TLS transport. Do not add `transport/tls.rs` or pretend TLS works.
- Do not implement TLS transport or standalone lock/unlock/delete CLI commands unless asked. Jump auth is IdentityFile or agent (device password is not reused).
- Do not bump crate versions or publish. release-please opens the release PR and tags on merge.
- Do not rewrite working serialize/framer tests to “simplify” them.
- Filter files are subtree XML. `Filter::subtree` is infallible and passes the XML through verbatim; it must never panic on caller input.
- Caller XML (`<config>`, `<filter>`) is spliced in after serialization via a placeholder, so it reaches the device byte for byte while `<url>` and friends stay escaped. Do not "simplify" this by unescaping the whole document.
