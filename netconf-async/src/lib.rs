#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod connection;
pub mod error;
pub mod framer;
pub mod message;
pub mod transport;

pub use connection::Connection;
pub use error::{NetconfClientError, NetconfClientResult};
pub use message::{Datastore, Filter, RpcReply};
pub use transport::Transport;

#[cfg(feature = "ssh")]
pub use transport::ssh::{
    HostKeyPolicy, JumpPool, JumpSession, SshAuth, SshConfig, SshJump, SshSessionOpts, SshTransport,
};

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
/// `:interleave:1.0` capability — RPCs on a session with an active subscription ([RFC5277](https://www.rfc-editor.org/rfc/rfc5277.html#section-6)).
pub const INTERLEAVE_CAP: &str = "urn:ietf:params:netconf:capability:interleave:1.0";
/// Default NETCONF-over-SSH port ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).
pub const DEFAULT_NETCONF_SSH_PORT: u16 = 830;
