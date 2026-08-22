//! Error type and `Result` alias for the client.

use crate::message;
use thiserror::Error;

/// `Result` alias used by the public API.
pub type NetconfClientResult<T> = Result<T, NetconfClientError>;

/// Transport, framing, XML, or NETCONF `<rpc-error>` failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NetconfClientError {
    /// I/O failure on the underlying stream.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SSH handshake, auth, or subsystem error.
    #[cfg(feature = "ssh")]
    #[cfg_attr(docsrs, doc(cfg(feature = "ssh")))]
    #[error("{0}")]
    Ssh(String),
    /// XML serialize or deserialize failure.
    #[error(transparent)]
    SerializingFailure(#[from] quick_xml::DeError),
    /// Device returned one or more `<rpc-error>` elements.
    #[error("remote procedure call failed:\n{0}")]
    Netconf(#[from] message::RpcReply),
    /// Datastore name was not `running`, `candidate`, `startup`, or a URL.
    #[error("unknown datastore {}, (expected {:?})", unknown, expected)]
    UnknownDatastore {
        /// Datastore names the parser accepts.
        expected: Vec<String>,
        /// The name that was supplied.
        unknown: String,
    },
    /// 1.1 chunk header did not match `\n#N\n` / `\n##\n`.
    #[error(
        "malformed message chunk (expected {:?}, actual {:?})",
        expected,
        actual
    )]
    MalformedChunk {
        /// Byte the framer required at this position.
        expected: char,
        /// Byte the device actually sent.
        actual: char,
    },
    /// Reply exceeded the framer's size limit.
    ///
    /// Raised for an over-long chunk header as well as for a body that grows
    /// past the limit. Tune with
    /// [`AsyncFramer::set_max_message_size`](crate::framer::async_framer::AsyncFramer::set_max_message_size).
    #[error("message exceeds the configured limit of {limit} bytes")]
    MessageTooLarge {
        /// Configured maximum, in bytes.
        limit: usize,
    },
    /// RPC exceeded [`Connection::set_timeout`](crate::connection::Connection::set_timeout).
    ///
    /// The reply may still be in flight, so the session is left unusable.
    #[cfg(feature = "tokio")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
    #[error("no reply within {}s; session is no longer usable", timeout.as_secs_f32())]
    Timeout {
        /// Configured per-RPC timeout.
        timeout: core::time::Duration,
    },
    /// An RPC was attempted after a timeout left the session out of sync.
    #[error("session is out of sync after an earlier timeout")]
    SessionDesynchronized,
    /// Reply `message-id` did not match the request ([RFC6241 4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.1)).
    ///
    /// The mismatched message has been consumed, so the real reply may still
    /// be in flight. The session is left unusable.
    #[error("rpc-reply message-id mismatch: expected {expected}, got {actual}")]
    MessageIdMismatch {
        /// `message-id` sent on the request.
        expected: String,
        /// `message-id` on the reply, or `<none>` if the device omitted it.
        actual: String,
    },
    /// Connection dropped after `<commit>` / `<commit-configuration>` was sent.
    ///
    /// The device may already have applied the change. This is not a clean
    /// I/O error — verify device state before retrying.
    #[error(
        "commit status unknown: connection lost after sending <commit>; the device may have committed — verify device state"
    )]
    CommitUnknown,
    /// Server host key was rejected by the configured [`crate::transport::ssh::HostKeyPolicy`].
    #[cfg(feature = "ssh")]
    #[cfg_attr(docsrs, doc(cfg(feature = "ssh")))]
    #[error("SSH host key for {host} rejected ({reason}); server presented {fingerprint}")]
    HostKeyRejected {
        /// Host whose key was checked.
        host: String,
        /// SHA-256 fingerprint the server presented (`SHA256:…`).
        fingerprint: String,
        /// Why the policy rejected it.
        reason: String,
    },
    /// Anything that does not fit the other variants.
    #[error("{0}")]
    Other(String),
}

impl NetconfClientError {
    /// Wrap an arbitrary message as [`NetconfClientError::Other`].
    pub fn new(msg: impl Into<String>) -> Self {
        NetconfClientError::Other(msg.into())
    }

    /// True when the transport hit EOF mid-read.
    pub(crate) fn is_unexpected_eof(&self) -> bool {
        matches!(self, Self::Io(err) if err.kind() == std::io::ErrorKind::UnexpectedEof)
    }
}
