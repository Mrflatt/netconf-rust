use crate::error::NetconfClientResult;
use async_trait::async_trait;

#[cfg(feature = "tokio")]
pub mod async_framer;

pub const NETCONF_1_0_TERMINATOR: &str = "]]>]]>";

/// Trait for NETCONF framer
#[async_trait]
pub trait Framer: Send {
    async fn upgrade(&mut self);
    async fn read_async(&mut self) -> NetconfClientResult<String>;
    async fn write_async(&mut self, rpc: &str) -> NetconfClientResult<()>;
}
