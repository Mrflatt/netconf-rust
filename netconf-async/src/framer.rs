//! NETCONF 1.0 end-of-message and 1.1 chunked framing ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).

use crate::error::NetconfClientResult;
use async_trait::async_trait;

#[cfg(feature = "tokio")]
pub mod async_framer;

/// 1.0 message terminator (`]]>]]>`).
pub const NETCONF_1_0_TERMINATOR: &str = "]]>]]>";

/// Encode and decode NETCONF messages on a byte stream.
#[async_trait]
pub trait Framer: Send {
    /// Switch from 1.0 (`]]>]]>`) to 1.1 chunked framing.
    async fn upgrade(&mut self);
    /// Read one framed message.
    async fn read_async(&mut self) -> NetconfClientResult<String>;
    /// Write one framed message.
    async fn write_async(&mut self, rpc: &str) -> NetconfClientResult<()>;
}
