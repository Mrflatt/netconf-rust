//! Session-level tests against a scripted in-memory device.

use std::sync::{Arc, Mutex};

use netconf_async::framer::Framer;
use netconf_async::framer::async_framer::AsyncFramer;
use netconf_async::message::EditContent;
use netconf_async::{Connection, Datastore, NetconfClientError, Transport};
use netconf_mcp::{AllowedNets, ConnectParams, DeviceConnect, McpConfig};
use tokio::io::DuplexStream;

struct MemoryTransport {
    framer: AsyncFramer<DuplexStream>,
    sent: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Transport for MemoryTransport {
    async fn receive(&mut self) -> Result<String, NetconfClientError> {
        self.framer.read_async().await
    }

    async fn write(&mut self, rpc: &str) -> Result<(), NetconfClientError> {
        self.sent.lock().unwrap().push(rpc.to_string());
        self.framer.write_async(rpc).await
    }

    async fn write_and_receive(&mut self, rpc: &str) -> Result<String, NetconfClientError> {
        self.write(rpc).await?;
        self.receive().await
    }

    async fn close(&mut self) -> Result<(), NetconfClientError> {
        Ok(())
    }

    async fn upgrade(&mut self) {
        self.framer.upgrade().await;
    }
}

enum Step {
    Reply(String),
    Upgrade,
}

struct ScriptedConnect {
    script: Vec<Step>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl Clone for ScriptedConnect {
    fn clone(&self) -> Self {
        Self {
            script: self
                .script
                .iter()
                .map(|step| match step {
                    Step::Reply(xml) => Step::Reply(xml.clone()),
                    Step::Upgrade => Step::Upgrade,
                })
                .collect(),
            sent: self.sent.clone(),
        }
    }
}

#[async_trait::async_trait]
impl DeviceConnect for ScriptedConnect {
    async fn connect(
        &self,
        _params: ConnectParams,
    ) -> netconf_async::NetconfClientResult<Connection> {
        let (client, server) = tokio::io::duplex(1 << 16);
        let sent = self.sent.clone();
        let script = self.clone().script;
        tokio::spawn(async move {
            let mut framer = AsyncFramer::new(server);
            for step in script {
                match step {
                    Step::Reply(reply) => {
                        let Ok(request) = framer.read_async().await else {
                            return;
                        };
                        let reply = echo_message_id(&request, &reply);
                        if framer.write_async(&reply).await.is_err() {
                            return;
                        }
                    }
                    Step::Upgrade => framer.upgrade().await,
                }
            }
        });
        Connection::new(MemoryTransport {
            framer: AsyncFramer::new(client),
            sent,
        })
        .await
    }
}

fn echo_message_id(request: &str, reply: &str) -> String {
    match (message_id_attr(request), message_id_attr(reply)) {
        (Some(req), Some(rep)) if req != rep => reply.replacen(
            &format!("message-id=\"{rep}\""),
            &format!("message-id=\"{req}\""),
            1,
        ),
        _ => reply.to_string(),
    }
}

fn message_id_attr(xml: &str) -> Option<&str> {
    let key = "message-id=\"";
    let start = xml.find(key)? + key.len();
    let end = xml[start..].find('"')?;
    Some(&xml[start..start + end])
}

fn hello() -> String {
    r#"<hello xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><capabilities>
<capability>urn:ietf:params:netconf:base:1.0</capability>
<capability>urn:ietf:params:netconf:base:1.1</capability>
<capability>urn:ietf:params:netconf:capability:candidate:1.0</capability>
<capability>urn:ietf:params:netconf:capability:writable-running:1.0</capability>
<capability>urn:ietf:params:netconf:capability:validate:1.1</capability>
<capability>urn:ietf:params:netconf:capability:rollback-on-error:1.0</capability>
<capability>urn:ietf:params:netconf:capability:startup:1.0</capability>
<capability>urn:ietf:params:netconf:capability:confirmed-commit:1.1</capability>
<capability>urn:ietf:params:netconf:capability:notification:1.0</capability>
</capabilities><session-id>7</session-id></hello>"#
        .to_string()
}

const OK: &str = r#"<rpc-reply message-id="1" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><ok/></rpc-reply>"#;

fn handshake_ok(n: usize) -> Vec<Step> {
    let mut script = vec![Step::Reply(hello()), Step::Upgrade];
    script.extend(std::iter::repeat_with(|| Step::Reply(OK.to_string())).take(n));
    script
}

#[tokio::test]
async fn connector_opens_session() {
    let connector = ScriptedConnect {
        script: handshake_ok(1),
        sent: Arc::new(Mutex::new(Vec::new())),
    };
    let mut conn = connector
        .connect(ConnectParams {
            host: "192.0.2.1".into(),
            username: None,
            password: None,
            timeout: None,
            allowed_subnets: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(conn.session_id(), Some(7));
    conn.close_session().await.unwrap();
}

#[tokio::test]
async fn edit_path_does_not_copy_startup_by_default() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let connector = ScriptedConnect {
        script: handshake_ok(6),
        sent: sent.clone(),
    };
    let mut conn = connector
        .connect(ConnectParams {
            host: "192.0.2.1".into(),
            username: None,
            password: None,
            timeout: None,
            allowed_subnets: Default::default(),
        })
        .await
        .unwrap();

    conn.lock(Datastore::Candidate).await.unwrap();
    conn.edit_config(
        Datastore::Candidate,
        EditContent::Config("<config><foo/></config>".into()),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    conn.validate(Datastore::Candidate).await.unwrap();
    conn.commit().await.unwrap();
    conn.unlock(Datastore::Candidate).await.unwrap();
    conn.close_session().await.unwrap();

    let sent = sent.lock().unwrap();
    let joined = sent.join("\n");
    assert!(joined.contains("<lock>"), "{joined}");
    assert!(joined.contains("<edit-config>"), "{joined}");
    assert!(joined.contains("<validate>"), "{joined}");
    assert!(joined.contains("<commit"), "{joined}");
    assert!(joined.contains("<unlock>"), "{joined}");
    assert!(!joined.contains("<copy-config>"), "{joined}");
}

#[test]
fn stdio_defaults_are_writable() {
    assert!(!McpConfig::stdio().read_only);
}

#[test]
fn allowed_nets_filter_hosts() {
    let nets = AllowedNets::parse(["10.0.0.0/8"]).unwrap();
    assert!(nets.check("10.1.2.3").is_ok());
    assert!(nets.check("192.0.2.1").is_err());
}
