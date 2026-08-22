//! SSH transport that requests the `netconf` subsystem ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).

use crate::error::{NetconfClientError, NetconfClientResult};
use crate::framer::Framer;
use crate::framer::async_framer::AsyncFramer;
use crate::transport::Transport;
use async_ssh2_lite::{AsyncChannel, AsyncSession, SessionConfiguration, ssh2};
use async_trait::async_trait;
use tokio::net::TcpStream;

/// NETCONF-over-SSH session on a Tokio [`TcpStream`].
///
/// Default NETCONF port is **830**; the caller puts it in `addr`.
pub struct SSHTransport {
    session: AsyncSession<TcpStream>,
    framer: AsyncFramer<AsyncChannel<TcpStream>>,
}

impl SSHTransport {
    /// Wrap an already-authenticated [`AsyncSession`] and request subsystem `netconf`.
    ///
    /// Use this for ssh-agent or public-key auth; build and authenticate the
    /// session yourself, then hand it over. Set any timeout on the session
    /// before calling, and bound individual RPCs with
    /// [`Connection::set_timeout`](crate::connection::Connection::set_timeout).
    pub async fn new_with_session(
        session: AsyncSession<TcpStream>,
    ) -> NetconfClientResult<SSHTransport> {
        connect_internal(session).await
    }

    /// TCP connect, password-authenticate, request subsystem `netconf`.
    ///
    /// `addr` is `host:port`, e.g. `192.0.2.10:830`.
    pub async fn new_with_user_auth(
        addr: &str,
        user_name: &str,
        password: &str,
    ) -> NetconfClientResult<SSHTransport> {
        let stream = TcpStream::connect(addr).await?;
        let mut configuration = SessionConfiguration::new();
        configuration.set_timeout(10000);
        let mut sess = AsyncSession::new(stream, configuration)?;
        sess.handshake().await?;

        sess.userauth_password(user_name, password).await?;
        connect_internal(sess).await
    }
}

#[async_trait]
impl Transport for SSHTransport {
    async fn receive(&mut self) -> NetconfClientResult<String> {
        self.framer.read_async().await
    }

    async fn write(&mut self, rpc: &str) -> NetconfClientResult<()> {
        self.framer.write_async(rpc).await
    }
    async fn write_and_receive(&mut self, rpc: &str) -> NetconfClientResult<String> {
        self.framer.write_async(rpc).await?;
        self.framer.read_async().await
    }

    async fn close(&mut self) -> NetconfClientResult<()> {
        let channel = self.framer.channel_mut();
        channel.send_eof().await?;
        channel.wait_eof().await?;
        channel.close().await?;
        channel.wait_close().await?;
        self.session
            .disconnect(Some(ssh2::ByApplication), "Shutdown", None)
            .await?;
        Ok(())
    }

    async fn upgrade(&mut self) {
        self.framer.upgrade().await;
    }
}

async fn connect_internal(session: AsyncSession<TcpStream>) -> NetconfClientResult<SSHTransport> {
    if session.authenticated() {
        let mut channel = session.channel_session().await?;
        channel.subsystem("netconf").await?;
        let transport = SSHTransport {
            session,
            framer: AsyncFramer::new(channel),
        };
        Ok(transport)
    } else {
        Err(NetconfClientError::new(
            "SSH session is not authenticated; authenticate before requesting the netconf subsystem",
        ))
    }
}
