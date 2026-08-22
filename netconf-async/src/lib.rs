//! Async [NETCONF](https://www.rfc-editor.org/rfc/rfc6241.html) client.
//!
//! Talk to routers and switches over SSH using the `netconf` subsystem. The
//! crate handles the `<hello>` exchange, base 1.0 / 1.1 framing, and typed RPCs.
//!
//! ```toml
//! [dependencies]
//! netconf-async = "0.1"
//! ```
//!
//! # Example
//!
//! ```no_run
//! use netconf_async::connection::Connection;
//! use netconf_async::message::{Datastore, Filter};
//! use netconf_async::transport::ssh::SSHTransport;
//!
//! # async fn run() -> netconf_async::error::NetconfClientResult<()> {
//! let transport =
//!     SSHTransport::new_with_user_auth("192.0.2.10:830", "netconf", "secret").await?;
//! let mut conn = Connection::new(transport).await?;
//!
//! let running = conn.get_config(Datastore::Running, None, None).await?;
//! println!("{running}");
//!
//! let filter = Filter::subtree(
//!     r#"<system xmlns="urn:ietf:params:xml:ns:yang:ietf-system"/>"#,
//! );
//! let filtered = conn.get(Some(filter), None).await?;
//! println!("{filtered}");
//!
//! conn.close_session().await?;
//! # Ok(())
//! # }
//! ```
//!
//! [`Connection::new`](connection::Connection::new) performs the `<hello>`
//! exchange and upgrades the framer when the server advertises
//! [`NETCONF_BASE_11_CAP`]. Skipping [`close_session`](connection::Connection::close_session)
//! still closes on [`Drop`] (tokio feature).
//!
//! Password auth is the short path. For ssh-agent or key files, build an
//! authenticated `async_ssh2_lite::AsyncSession` and wrap it with
//! [`SSHTransport::new_with_session`](transport::ssh::SSHTransport::new_with_session).
//! A custom byte pipe implements [`Transport`](transport::Transport).
//!
//! # Session flow
//!
//! 1. SSH connect, request subsystem `netconf`.
//! 2. Exchange `<hello>` with **1.0** framing (`]]>]]>`).
//! 3. If the server advertises [`NETCONF_BASE_11_CAP`], upgrade to chunked
//!    framing ([RFC 6242](https://www.rfc-editor.org/rfc/rfc6242.html)).
//! 4. Each RPC is written and the reply parsed as
//!    [`RpcReply`](message::RpcReply); any `<rpc-error>` becomes
//!    [`NetconfClientError::Netconf`](error::NetconfClientError::Netconf).
//! 5. [`close_session`](connection::Connection::close_session) marks the
//!    session closed so `Drop` does not send another `<close-session>`.
//!
//! Default NETCONF-over-SSH port is **830**.
//!
//! Notifications use `<create-subscription>`
//! ([RFC 5277](https://www.rfc-editor.org/rfc/rfc5277.html)).
//! `with-defaults` follows [RFC 6243](https://www.rfc-editor.org/rfc/rfc6243.html).

pub mod connection;
pub mod error;
pub mod framer;
pub mod message;
pub mod transport;

/// XML namespace for NETCONF 1.0 messages ([RFC6241](https://www.rfc-editor.org/rfc/rfc6241.html)).
pub const NETCONF_URN: &str = "urn:ietf:params:xml:ns:netconf:base:1.0";
/// `:base:1.0` capability ([RFC6241](https://www.rfc-editor.org/rfc/rfc6241.html)).
pub const NETCONF_BASE_10_CAP: &str = "urn:ietf:params:netconf:base:1.0";
/// `:base:1.1` capability; advertised by the server when chunked framing is supported ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).
pub const NETCONF_BASE_11_CAP: &str = "urn:ietf:params:netconf:base:1.1";
/// `:writable-running:1.0` capability ([RFC6241 8.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.2)).
pub const WRITABLE_RUNNING_CAP: &str = "urn:ietf:params:netconf:capability:writable-running:1.0";
/// `:candidate:1.0` capability ([RFC6241 8.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3)).
pub const CANDIDATE_CAP: &str = "urn:ietf:params:netconf:capability:candidate:1.0";
/// `:startup:1.0` capability ([RFC6241 8.7](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.7)).
pub const STARTUP_CAP: &str = "urn:ietf:params:netconf:capability:startup:1.0";
/// `:rollback-on-error:1.0` capability ([RFC6241 8.5](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.5)).
pub const ROLLBACK_ON_ERROR_CAP: &str = "urn:ietf:params:netconf:capability:rollback-on-error:1.0";
/// `:url:1.0` capability ([RFC6241 8.8](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.8)).
pub const URL_CAP: &str = "urn:ietf:params:netconf:capability:url:1.0";
/// `:confirmed-commit:1.0` capability ([RFC6241 8.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4)).
pub const CONFIRMED_COMMIT_10_CAP: &str = "urn:ietf:params:netconf:capability:confirmed-commit:1.0";
/// `:confirmed-commit:1.1` capability ([RFC6241 8.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4)).
pub const CONFIRMED_COMMIT_CAP: &str = "urn:ietf:params:netconf:capability:confirmed-commit:1.1";
/// `:validate:1.0` capability ([RFC6241 8.6](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.6)).
pub const VALIDATE_10_CAP: &str = "urn:ietf:params:netconf:capability:validate:1.0";
/// `:validate:1.1` capability ([RFC6241 8.6](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.6)).
pub const VALIDATE_CAP: &str = "urn:ietf:params:netconf:capability:validate:1.1";
/// `:with-defaults:1.0` capability ([RFC6243](https://www.rfc-editor.org/rfc/rfc6243.html)).
pub const WITH_DEFAULTS_CAP: &str = "urn:ietf:params:netconf:capability:with-defaults:1.0";
/// `:notification:1.0` capability ([RFC5277](https://www.rfc-editor.org/rfc/rfc5277.html)).
pub const NOTIFICATION_CAP: &str = "urn:ietf:params:netconf:capability:notification:1.0";
