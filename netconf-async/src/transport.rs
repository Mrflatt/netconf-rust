//! Byte transport under the framer (SSH today).

use crate::error::NetconfClientResult;
use async_trait::async_trait;

#[cfg(feature = "ssh")]
#[cfg_attr(docsrs, doc(cfg(feature = "ssh")))]
pub mod ssh;

/// Byte pipe that carries framed NETCONF messages.
///
/// Implement this for a custom channel and pass it to
/// [`Connection::new`](crate::connection::Connection::new).
#[async_trait]
pub trait Transport: Send {
    /// Read one framed message.
    async fn receive(&mut self) -> NetconfClientResult<String>;
    /// Write one framed message.
    async fn write(&mut self, rpc: &str) -> NetconfClientResult<()>;
    /// Write one message and read the reply.
    async fn write_and_receive(&mut self, rpc: &str) -> NetconfClientResult<String>;
    /// Tear down the underlying channel.
    async fn close(&mut self) -> NetconfClientResult<()>;
    /// Switch from 1.0 end-of-message framing to 1.1 chunks ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).
    async fn upgrade(&mut self);
}
