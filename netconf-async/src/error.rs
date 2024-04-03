use crate::message;
use thiserror::Error;

pub type NetconfClientResult<T> = Result<T, NetconfClientError>;

#[derive(Debug, Error)]
pub enum NetconfClientError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(feature = "async-ssh2-lite")]
    #[error(transparent)]
    Ssh(#[from] async_ssh2_lite::Error),
    #[error(transparent)]
    SerializingFailure(#[from] quick_xml::DeError),
    #[error("remote procedure call failed:\n{0}")]
    Netconf(#[from] message::RpcReply),
    #[error("unknown datastore {}, (expected {:?})", unknown, expected)]
    UnknownDatastore {
        expected: Vec<String>,
        unknown: String,
    },
    #[error(
        "malformed message chunk (expected {:?}, actual {:?})",
        expected,
        actual
    )]
    MalformedChunk { expected: char, actual: char },
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl NetconfClientError {
    pub fn new(msg: String) -> Self {
        NetconfClientError::Anyhow(anyhow::Error::msg(msg))
    }
}
