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
    #[error(transparent)]
    Ssh(#[from] async_ssh2_lite::Error),
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
    /// Anything that does not fit the other variants.
    #[error("{0}")]
    Other(String),
}

impl NetconfClientError {
    /// Wrap an arbitrary message as [`NetconfClientError::Other`].
    pub fn new(msg: impl Into<String>) -> Self {
        NetconfClientError::Other(msg.into())
    }
}
