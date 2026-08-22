//! Error type and `Result` alias for the client.

use crate::message;
use thiserror::Error;

/// `Result` alias used by the public API.
pub type NetconfClientResult<T> = Result<T, NetconfClientError>;

/// Transport, framing, XML, or NETCONF `<rpc-error>` failure.
#[derive(Debug, Error)]
pub enum NetconfClientError {
    /// I/O failure on the underlying stream.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SSH handshake, auth, or subsystem error.
    #[cfg(feature = "async-ssh2-lite")]
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
        expected: Vec<String>,
        unknown: String,
    },
    /// 1.1 chunk header did not match `\n#N\n` / `\n##\n`.
    #[error(
        "malformed message chunk (expected {:?}, actual {:?})",
        expected,
        actual
    )]
    MalformedChunk { expected: char, actual: char },
    /// Catch-all for stringly errors that do not fit the other variants.
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl NetconfClientError {
    /// Wrap an arbitrary message as [`NetconfClientError::Anyhow`].
    pub fn new(msg: String) -> Self {
        NetconfClientError::Anyhow(anyhow::Error::msg(msg))
    }
}
