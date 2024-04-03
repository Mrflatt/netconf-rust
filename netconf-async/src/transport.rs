use crate::error::NetconfClientResult;
use async_trait::async_trait;

#[cfg(feature = "async-ssh2-lite")]
pub mod ssh;

/// Trait for NETCONF transport
#[async_trait]
pub trait Transport: Send {
    async fn receive(&mut self) -> NetconfClientResult<String>;
    async fn write(&mut self, rpc: &str) -> NetconfClientResult<()>;
    async fn write_and_receive(&mut self, rpc: &str) -> NetconfClientResult<String>;
    async fn close(&mut self) -> NetconfClientResult<()>;
    async fn upgrade(&mut self);
}
