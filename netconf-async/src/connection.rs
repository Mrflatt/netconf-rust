use crate::error::{NetconfClientError, NetconfClientResult};
use crate::message::{
    Datastore, Filter, Hello, Rpc, RpcOperation, RpcReply, Source, WithDefaultsValue,
};
use crate::transport::Transport;
use crate::NETCONF_BASE_11_CAP;
use core::time::Duration;
use log::{debug, error};
use quick_xml::de::from_str;
#[cfg(feature = "tokio")]
use tokio::runtime::Handle;
#[cfg(feature = "tokio")]
use tokio::sync::mpsc::Sender;
#[cfg(feature = "tokio")]
use tokio::task::block_in_place;
#[cfg(feature = "tokio")]
use tokio::{select, signal};

pub struct Connection {
    pub(crate) transport: Box<dyn Transport + Send + 'static>,

    session_id: Option<u64>,
    skip_serializing: bool,
    is_closed: bool,
}

impl Connection {
    pub async fn new<T>(transport: T) -> NetconfClientResult<Connection>
    where
        T: Transport + 'static,
    {
        let mut conn = Connection {
            transport: Box::from(transport),
            session_id: None,
            skip_serializing: false,
            is_closed: false,
        };
        conn.session_id = conn.hello().await?;
        Ok(conn)
    }

    pub fn set_skip_serializing(&mut self) {
        self.skip_serializing = true
    }

    pub fn session_id(&self) -> u64 {
        self.session_id.unwrap_or(0)
    }

    async fn hello(&mut self) -> NetconfClientResult<Option<u64>> {
        let hello = Hello::new();
        let response = self.transport.write_and_receive(&hello.to_string()).await?;
        debug!("Hello:\n{}", response);

        let hello: Hello = from_str(&response)?;
        if hello.has_capability(NETCONF_BASE_11_CAP) {
            self.transport.upgrade().await;
        }
        Ok(hello.session_id())
    }

    /// GetConfig implements the `<get-config>` rpc operation defined in [RFC6241 7.1].
    /// `source` is the datastore to query.
    ///
    /// [RFC6241 7.1]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.1
    pub async fn get_config(
        &mut self,
        datastore: Datastore,
        filter: Option<Filter>,
        defaults: Option<WithDefaultsValue>,
    ) -> NetconfClientResult<String> {
        let get_config =
            Rpc::new_with_operation(RpcOperation::new_get_config(datastore, filter, defaults));
        self.run_rpc(get_config).await
    }

    pub async fn get(
        &mut self,
        filter: Option<Filter>,
        defaults: Option<WithDefaultsValue>,
    ) -> NetconfClientResult<String> {
        let get_config = Rpc::new_with_operation(RpcOperation::new_get(filter, defaults));
        self.run_rpc(get_config).await
    }

    pub async fn validate(&mut self, datastore: Datastore) -> NetconfClientResult<String> {
        let validate = Rpc::new_with_operation(RpcOperation::Validate {
            source: Source { datastore },
        });
        self.run_rpc(validate).await
    }

    pub async fn commit(&mut self) -> NetconfClientResult<String> {
        let commit = Rpc::new_with_operation(RpcOperation::new_commit(None, None, None, None));
        self.run_rpc(commit).await
    }

    pub async fn confirmed_commit(
        &mut self,
        confirm_timeout: Option<i32>,
        persist: Option<String>,
        persist_id: Option<String>,
    ) -> NetconfClientResult<String> {
        let commit = Rpc::new_with_operation(RpcOperation::new_commit(
            Some(()),
            confirm_timeout,
            persist,
            persist_id,
        ));
        self.run_rpc(commit).await
    }

    pub async fn close_session(&mut self) -> NetconfClientResult<String> {
        let close_session = Rpc::new_with_operation(RpcOperation::CloseSession);
        self.is_closed = true;
        self.run_rpc(close_session).await
    }

    pub async fn kill_session(&mut self, session_id: u64) -> NetconfClientResult<String> {
        let kill_session = Rpc::new_with_operation(RpcOperation::KillSession { session_id });
        self.is_closed = true;
        self.run_rpc(kill_session).await
    }

    /// Issues the `<create-subscription>` operation as defined in [RFC5277 2.1.1](https://www.rfc-editor.org/rfc/rfc5277.html#section-2.1.1)
    /// for initiating an event notification subscription that will send asynchronous event notifications to the initiator.
    ///
    /// This requires the device to support the [notification capability](https://www.rfc-editor.org/rfc/rfc5277.html#section-3.1.1)
    ///
    /// It is caller responsibility to handle the notifications stream.
    #[cfg(feature = "tokio")]
    pub async fn notification(
        &mut self,
        sender: Sender<String>,
        stream: Option<&str>,
        duration: Option<Duration>,
    ) -> NetconfClientResult<()> {
        let notification = Rpc::new_with_operation(RpcOperation::new_create_subscription(
            stream, None, duration,
        ));
        self.run_rpc(notification).await?;
        self.run_notification_loop(sender).await
    }

    #[cfg(feature = "tokio")]
    async fn run_notification_loop(&mut self, sender: Sender<String>) -> NetconfClientResult<()> {
        select! {
            result = async {
                if let Err(err) = signal::ctrl_c().await {
                    Err(NetconfClientError::Io(err))
                } else {
                    Ok(())
                }
            } => {
                result
            }
            result = async {
                loop {
                    match self.transport.receive().await {
                        Ok(resp) => {
                            if let Err(err) = sender.send(resp).await {
                                break Err(NetconfClientError::new(format!("send error: {}", err)));
                            }
                        }
                        Err(err) => {
                            break Err(err);
                        }
                    };
                }
            } => {
                result
            }
        }
    }

    async fn run_rpc(&mut self, rpc: Rpc) -> NetconfClientResult<String> {
        let response = self.transport.write_and_receive(&rpc.to_string()).await?;
        debug!("RPC:\n{}", response);

        if !self.skip_serializing {
            let reply: RpcReply = from_str(&response)?;
            if reply.has_errors() {
                return Err(NetconfClientError::Netconf(reply));
            }
        }
        Ok(response)
    }
}

#[cfg(feature = "tokio")]
impl Drop for Connection {
    fn drop(&mut self) {
        if !self.is_closed {
            block_in_place(|| {
                Handle::current().block_on(async {
                    if let Err(err) = self.close_session().await {
                        error!("Error closing netconf session: {}", err);
                    }
                });
            });
        }
    }
}
