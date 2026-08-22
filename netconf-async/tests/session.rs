//! Session-level tests driven by a scripted in-memory device.

use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientError;
use netconf_async::framer::Framer;
use netconf_async::framer::async_framer::AsyncFramer;
use netconf_async::message::{Datastore, ErrorTag, Filter};
use netconf_async::transport::Transport;
use std::sync::{Arc, Mutex};
use tokio::io::DuplexStream;

/// Transport over a duplex pipe, recording everything the client sends.
struct MemoryTransport {
    framer: AsyncFramer<DuplexStream>,
    sent: Arc<Mutex<Vec<String>>>,
    closed: Arc<Mutex<bool>>,
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
        *self.closed.lock().unwrap() = true;
        Ok(())
    }

    async fn upgrade(&mut self) {
        self.framer.upgrade().await;
    }
}

/// One step in a scripted device conversation.
enum Step {
    /// Read a request, then answer it.
    Reply(String),
    /// Send an unsolicited message, e.g. a notification.
    Push(String),
    /// Switch to 1.1 chunked framing.
    Upgrade,
    /// Stay connected but never answer again.
    GoQuiet,
}

/// A device that walks a script, framed the way it announced.
struct Device {
    transport: MemoryTransport,
    sent: Arc<Mutex<Vec<String>>>,
    closed: Arc<Mutex<bool>>,
}

fn device(script: Vec<Step>) -> Device {
    let (client, server) = tokio::io::duplex(1 << 16);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(false));

    tokio::spawn(async move {
        let mut framer = AsyncFramer::new(server);
        for step in script {
            match step {
                Step::Reply(reply) => {
                    // Read the request first so the pipe cannot fill up.
                    if framer.read_async().await.is_err()
                        || framer.write_async(&reply).await.is_err()
                    {
                        return;
                    }
                }
                Step::Push(message) => {
                    if framer.write_async(&message).await.is_err() {
                        return;
                    }
                }
                Step::Upgrade => framer.upgrade().await,
                // Hold the stream open so the client waits rather than sees EOF.
                Step::GoQuiet => std::future::pending::<()>().await,
            }
        }
    });

    Device {
        transport: MemoryTransport {
            framer: AsyncFramer::new(client),
            sent: sent.clone(),
            closed: closed.clone(),
        },
        sent,
        closed,
    }
}

/// The common opening: answer the hello, then switch to chunked framing.
fn handshake(base_11: bool) -> Vec<Step> {
    let mut script = vec![Step::Reply(hello(base_11))];
    if base_11 {
        script.push(Step::Upgrade);
    }
    script
}

fn hello(base_11: bool) -> String {
    let extra = if base_11 {
        "<capability>urn:ietf:params:netconf:base:1.1</capability>"
    } else {
        ""
    };
    format!(
        r#"<hello xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><capabilities>
<capability>urn:ietf:params:netconf:base:1.0</capability>{extra}
<capability>urn:ietf:params:netconf:capability:url:1.0?scheme=http,ftp</capability>
</capabilities><session-id>7</session-id></hello>"#
    )
}

const OK_REPLY: &str = r#"<rpc-reply message-id="1" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><ok/></rpc-reply>"#;

#[tokio::test]
async fn hello_exchange_reports_session_and_capabilities() {
    let mut script = handshake(true);
    script.push(Step::Reply(OK_REPLY.to_string()));
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();

    assert_eq!(conn.session_id(), Some(7));
    assert!(conn.has_capability("urn:ietf:params:netconf:base:1.1"));
    // Query parameters must not defeat the lookup.
    assert!(conn.has_capability("urn:ietf:params:netconf:capability:url:1.0"));
    assert!(!conn.has_capability("urn:ietf:params:netconf:capability:candidate:1.0"));

    conn.close_session().await.unwrap();
    assert!(*device.closed.lock().unwrap(), "transport was not closed");
}

#[tokio::test]
async fn session_without_base_11_stays_on_end_of_message_framing() {
    let device = device(vec![
        Step::Reply(hello(false)),
        Step::Reply(OK_REPLY.to_string()),
    ]);
    let mut conn = Connection::new(device.transport).await.unwrap();
    conn.commit().await.unwrap();
    assert_eq!(conn.session_id(), Some(7));
}

#[tokio::test]
async fn namespace_prefixed_device_completes_the_handshake() {
    let prefixed = r#"<nc:hello xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
  <nc:capabilities>
    <nc:capability>urn:ietf:params:netconf:base:1.0</nc:capability>
    <nc:capability>urn:ietf:params:netconf:base:1.1</nc:capability>
  </nc:capabilities>
  <nc:session-id>31</nc:session-id>
</nc:hello>"#;
    let reply = r#"<nc:rpc-reply xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0" message-id="1"><nc:ok/></nc:rpc-reply>"#;
    let device = device(vec![
        Step::Reply(prefixed.to_string()),
        Step::Upgrade,
        Step::Reply(reply.to_string()),
    ]);

    let mut conn = Connection::new(device.transport).await.unwrap();
    assert_eq!(conn.session_id(), Some(31));
    conn.commit().await.unwrap();
}

#[tokio::test]
async fn rpc_error_surfaces_a_typed_tag() {
    let error = r#"<rpc-reply message-id="1" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <rpc-error>
    <error-type>protocol</error-type>
    <error-tag>lock-denied</error-tag>
    <error-severity>error</error-severity>
    <error-message>Lock held by another session</error-message>
    <error-info><session-id>4</session-id></error-info>
  </rpc-error>
</rpc-reply>"#;
    let mut script = handshake(true);
    script.push(Step::Reply(error.to_string()));
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();

    let err = conn.lock(Datastore::Candidate).await.unwrap_err();
    let NetconfClientError::Netconf(reply) = err else {
        panic!("expected a NETCONF error, got {err:?}");
    };
    assert_eq!(reply.errors().len(), 1);
    assert_eq!(reply.errors()[0].error_tag, ErrorTag::LockDenied);
    assert_eq!(
        reply.errors()[0].error_info.as_ref().unwrap().session_id,
        Some(4)
    );
}

#[tokio::test]
async fn parse_replies_off_returns_raw_xml_for_errors() {
    let error = r#"<rpc-reply message-id="1"><rpc-error><error-type>protocol</error-type><error-tag>in-use</error-tag><error-severity>error</error-severity></rpc-error></rpc-reply>"#;
    let mut script = handshake(true);
    script.push(Step::Reply(error.to_string()));
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();
    conn.set_parse_replies(false);

    let raw = conn.lock(Datastore::Candidate).await.unwrap();
    assert!(raw.contains("in-use"));
}

#[tokio::test]
async fn filter_and_config_payloads_go_out_verbatim() {
    let data = r#"<rpc-reply message-id="1"><data><top/></data></rpc-reply>"#;
    let mut script = handshake(true);
    script.push(Step::Reply(data.to_string()));
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();

    let filter = r#"<system xmlns="urn:ietf:params:xml:ns:yang:ietf-system"><name>a &amp; b</name></system>"#;
    conn.get_config(Datastore::Running, Some(Filter::subtree(filter)), None)
        .await
        .unwrap();

    let sent = device.sent.lock().unwrap();
    let request = sent.last().unwrap();
    assert!(request.contains(filter), "filter was rewritten:\n{request}");
}

#[tokio::test]
async fn url_datastore_keeps_its_ampersands_escaped() {
    let mut script = handshake(true);
    script.push(Step::Reply(OK_REPLY.to_string()));
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();

    conn.copy_config(
        netconf_async::message::CopySource::Running,
        Datastore::Url("ftp://host/cfg?user=a&pass=b".to_string()),
    )
    .await
    .unwrap();

    let sent = device.sent.lock().unwrap();
    let request = sent.last().unwrap();
    assert!(
        request.contains("<url>ftp://host/cfg?user=a&amp;pass=b</url>"),
        "url was not escaped:\n{request}"
    );
}

#[tokio::test]
async fn device_that_hangs_up_mid_session_reports_eof() {
    // Only the hello is answered; the pipe closes afterwards.
    let device = device(vec![Step::Reply(hello(false))]);
    let mut conn = Connection::new(device.transport).await.unwrap();

    let err = conn.commit().await.unwrap_err();
    assert!(
        matches!(err, NetconfClientError::Io(_)),
        "expected an I/O error once the device hung up, got {err:?}"
    );
}

#[tokio::test]
async fn silent_device_hits_the_rpc_timeout_and_desynchronizes() {
    let mut script = handshake(true);
    script.push(Step::GoQuiet);
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();
    conn.set_timeout(Some(std::time::Duration::from_millis(100)));

    let err = conn.commit().await.unwrap_err();
    assert!(
        matches!(err, NetconfClientError::Timeout { .. }),
        "expected a timeout, got {err:?}"
    );

    // The session is no longer trustworthy, so further RPCs are refused.
    let err = conn.commit().await.unwrap_err();
    assert!(
        matches!(err, NetconfClientError::SessionDesynchronized),
        "expected desynchronized, got {err:?}"
    );
}

#[tokio::test]
async fn notifications_stop_when_the_receiver_is_dropped() {
    let notification = r#"<notification xmlns="urn:ietf:params:xml:ns:netconf:notification:1.0"><eventTime>2026-01-01T00:00:00Z</eventTime><event/></notification>"#;
    let mut script = handshake(true);
    script.extend([
        Step::Reply(OK_REPLY.to_string()),
        Step::Push(notification.to_string()),
        Step::Push(notification.to_string()),
        Step::GoQuiet,
    ]);
    let device = device(script);
    let mut conn = Connection::new(device.transport).await.unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
    let received = tokio::spawn(async move {
        let first = rx.recv().await;
        drop(rx);
        first
    });

    conn.notification(tx, Some("NETCONF"), None, None)
        .await
        .unwrap();
    assert!(received.await.unwrap().unwrap().contains("eventTime"));
}
