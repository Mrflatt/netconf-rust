//! NETCONF session: hello exchange, typed RPCs, and teardown.

use crate::NETCONF_BASE_11_CAP;
use crate::error::{NetconfClientError, NetconfClientResult};
use crate::message::{
    CopySource, Datastore, DefaultOperation, EditContent, ErrorOption, Filter, Hello, Rpc,
    RpcOperation, RpcReply, Source, TestOption, WithDefaultsValue, capability_id, from_xml,
};
use crate::transport::Transport;
#[cfg(feature = "tokio")]
use core::time::Duration;
use log::{debug, warn};
#[cfg(feature = "tokio")]
use tokio::sync::mpsc::Sender;

/// Active NETCONF session on top of a [`Transport`].
///
/// Constructed via [`Connection::new`], which exchanges `<hello>` and upgrades
/// to 1.1 framing when the server advertises [`crate::NETCONF_BASE_11_CAP`].
///
/// Call [`Connection::close_session`] when finished; dropping a session that is
/// still open only logs a warning, because a clean shutdown needs to await I/O.
pub struct Connection {
    pub(crate) transport: Box<dyn Transport + Send + 'static>,

    session_id: Option<u64>,
    capabilities: Vec<String>,
    parse_replies: bool,
    warnings_as_errors: bool,
    next_message_id: u64,
    is_closed: bool,
    desynchronized: bool,
    #[cfg(feature = "tokio")]
    timeout: Option<Duration>,
}

impl Connection {
    /// Open a session: send client `<hello>`, parse the server hello, upgrade
    /// the framer if the server advertised `:base:1.1`.
    pub async fn new<T>(transport: T) -> NetconfClientResult<Connection>
    where
        T: Transport + 'static,
    {
        let mut conn = Connection {
            transport: Box::from(transport),
            session_id: None,
            capabilities: Vec::new(),
            parse_replies: true,
            warnings_as_errors: false,
            next_message_id: 1,
            is_closed: false,
            desynchronized: false,
            #[cfg(feature = "tokio")]
            timeout: None,
        };
        conn.session_id = conn.hello().await?;
        Ok(conn)
    }

    /// Whether replies are parsed as [`RpcReply`]; on by default.
    ///
    /// With parsing off, [`raw_rpc`](Self::raw_rpc) and the typed operations
    /// return the device's XML untouched and an `<rpc-error>` is *not* turned
    /// into [`NetconfClientError::Netconf`]. `message-id` is still checked
    /// when the reply can be parsed.
    pub fn set_parse_replies(&mut self, parse_replies: bool) {
        self.parse_replies = parse_replies;
    }

    /// True when replies are parsed and error-severity `<rpc-error>` is surfaced.
    pub fn parse_replies(&self) -> bool {
        self.parse_replies
    }

    /// Treat warning-severity `<rpc-error>` as [`NetconfClientError::Netconf`].
    ///
    /// Off by default: `<rpc-error severity="warning">` plus `<ok/>` is
    /// success, and the warnings stay in the returned XML (or come back from
    /// [`Self::raw_rpc_with_warnings`]). Turn this on to fail the RPC instead.
    pub fn set_warnings_as_errors(&mut self, warnings_as_errors: bool) {
        self.warnings_as_errors = warnings_as_errors;
    }

    /// True when warning-severity `<rpc-error>` fails the RPC.
    pub fn warnings_as_errors(&self) -> bool {
        self.warnings_as_errors
    }

    /// Fail an RPC that gets no reply within `timeout`; unlimited by default.
    ///
    /// A timed-out RPC leaves the session unusable, because the reply may still
    /// arrive and would be read as the answer to the next request. Later calls
    /// fail with [`NetconfClientError::SessionDesynchronized`].
    #[cfg(feature = "tokio")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    /// Server-assigned session-id from `<hello>`, if the server sent one.
    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }

    /// Server capabilities from the `<hello>` exchange.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// True if the server advertised `capability` (query string ignored).
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|cap| capability_id(cap) == capability)
    }

    async fn hello(&mut self) -> NetconfClientResult<Option<u64>> {
        let hello = Hello::new();
        let response = self.write_and_receive(&hello.to_string(), false).await?;
        debug!("Hello:\n{}", response);

        let hello: Hello = from_xml(&response)?;
        self.capabilities = hello.capabilities();
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

    /// `<get>` operational state ([RFC6241 7.7](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.7)).
    pub async fn get(
        &mut self,
        filter: Option<Filter>,
        defaults: Option<WithDefaultsValue>,
    ) -> NetconfClientResult<String> {
        let get = Rpc::new_with_operation(RpcOperation::new_get(filter, defaults));
        self.run_rpc(get).await
    }

    /// `<validate>` ([RFC6241 8.6.4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.6.4.1)).
    pub async fn validate(&mut self, datastore: Datastore) -> NetconfClientResult<String> {
        let validate = Rpc::new_with_operation(RpcOperation::Validate {
            source: Source { datastore },
        });
        self.run_rpc(validate).await
    }

    /// EditConfig implements the `<edit-config>` rpc operation defined in [RFC6241 7.2].
    ///
    /// [RFC6241 7.2]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2
    pub async fn edit_config(
        &mut self,
        target: Datastore,
        content: EditContent,
        default_operation: Option<DefaultOperation>,
        test_option: Option<TestOption>,
        error_option: Option<ErrorOption>,
    ) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_edit_config(
            target,
            content,
            default_operation,
            test_option,
            error_option,
        ));
        self.run_rpc(rpc).await
    }

    /// CopyConfig implements the `<copy-config>` rpc operation defined in [RFC6241 7.3].
    ///
    /// [RFC6241 7.3]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.3
    pub async fn copy_config(
        &mut self,
        source: CopySource,
        target: Datastore,
    ) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_copy_config(source, target));
        self.run_rpc(rpc).await
    }

    /// DeleteConfig implements the `<delete-config>` rpc operation defined in [RFC6241 7.4].
    ///
    /// [RFC6241 7.4]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.4
    pub async fn delete_config(&mut self, target: Datastore) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_delete_config(target));
        self.run_rpc(rpc).await
    }

    /// Lock implements the `<lock>` rpc operation defined in [RFC6241 7.5].
    ///
    /// [RFC6241 7.5]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.5
    pub async fn lock(&mut self, target: Datastore) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_lock(target));
        self.run_rpc(rpc).await
    }

    /// Unlock implements the `<unlock>` rpc operation defined in [RFC6241 7.6].
    ///
    /// [RFC6241 7.6]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.6
    pub async fn unlock(&mut self, target: Datastore) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_unlock(target));
        self.run_rpc(rpc).await
    }

    /// `<commit>` of the candidate datastore ([RFC6241 8.3.4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3.4.1)).
    pub async fn commit(&mut self) -> NetconfClientResult<String> {
        let commit = Rpc::new_with_operation(RpcOperation::new_commit(None, None, None, None));
        self.run_rpc(commit).await
    }

    /// Confirmed `<commit>` ([RFC6241 8.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4)).
    ///
    /// `persist` lets another session confirm later via [`Self::confirm_commit`].
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

    /// Confirm a previous persist confirmed-commit ([RFC6241 8.4.4.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4.4.2)).
    pub async fn confirm_commit(&mut self, persist_id: String) -> NetconfClientResult<String> {
        let commit =
            Rpc::new_with_operation(RpcOperation::new_commit(None, None, None, Some(persist_id)));
        self.run_rpc(commit).await
    }

    /// CancelCommit implements the `<cancel-commit>` rpc operation defined in [RFC6241 8.4.4.1].
    ///
    /// [RFC6241 8.4.4.1]: https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4.4.1
    pub async fn cancel_commit(
        &mut self,
        persist_id: Option<String>,
    ) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_cancel_commit(persist_id));
        self.run_rpc(rpc).await
    }

    /// DiscardChanges implements the `<discard-changes>` rpc operation defined in [RFC6241 8.3.4.2].
    ///
    /// [RFC6241 8.3.4.2]: https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3.4.2
    pub async fn discard_changes(&mut self) -> NetconfClientResult<String> {
        let rpc = Rpc::new_with_operation(RpcOperation::new_discard_changes());
        self.run_rpc(rpc).await
    }

    /// `<close-session>` ([RFC6241 7.8](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.8)).
    ///
    /// Also tears the transport down. A transport that fails to close is logged
    /// rather than returned, because the device has already ended the session.
    pub async fn close_session(&mut self) -> NetconfClientResult<String> {
        let close_session = Rpc::new_with_operation(RpcOperation::CloseSession);
        self.is_closed = true;
        let reply = self.run_rpc(close_session).await;
        if let Err(err) = self.transport.close().await {
            debug!("Transport close after <close-session> failed: {}", err);
        }
        reply
    }

    /// KillSession implements the `<kill-session>` rpc operation defined in [RFC6241 7.9].
    /// Terminates another NETCONF session; this connection stays open.
    ///
    /// [RFC6241 7.9]: https://www.rfc-editor.org/rfc/rfc6241.html#section-7.9
    pub async fn kill_session(&mut self, session_id: u64) -> NetconfClientResult<String> {
        let kill_session = Rpc::new_with_operation(RpcOperation::KillSession { session_id });
        self.run_rpc(kill_session).await
    }

    /// Sends a raw XML RPC document and returns the reply.
    pub async fn raw_rpc(&mut self, xml: &str) -> NetconfClientResult<String> {
        self.exchange(
            xml,
            request_message_id(xml).as_deref(),
            request_is_commit(xml),
            self.warnings_as_errors,
        )
        .await
    }

    /// Like [`Self::raw_rpc`], but returns warning-severity `<rpc-error>`s
    /// alongside the raw reply XML.
    ///
    /// Error-severity replies still become [`NetconfClientError::Netconf`]
    /// (unless [`Self::set_parse_replies`] is off). Warning-only replies
    /// succeed even when [`Self::set_warnings_as_errors`] is on, because this
    /// method exists to collect them.
    pub async fn raw_rpc_with_warnings(
        &mut self,
        xml: &str,
    ) -> NetconfClientResult<(String, Vec<crate::message::Error>)> {
        let response = self
            .exchange(
                xml,
                request_message_id(xml).as_deref(),
                request_is_commit(xml),
                false,
            )
            .await?;
        let warnings = match from_xml::<RpcReply>(&response) {
            Ok(reply) => reply.warnings().into_iter().cloned().collect(),
            Err(_) => Vec::new(),
        };
        Ok((response, warnings))
    }

    /// Issues the `<create-subscription>` operation as defined in [RFC5277 2.1.1](https://www.rfc-editor.org/rfc/rfc5277.html#section-2.1.1)
    /// for initiating an event notification subscription that will send asynchronous event notifications to the initiator.
    ///
    /// This requires the device to support the [notification capability](https://www.rfc-editor.org/rfc/rfc5277.html#section-3.1.1).
    ///
    /// Forwards notifications to `sender` until the device closes the stream or
    /// the receiver is dropped. The returned future is cancel-safe to drop, so
    /// wire up shutdown with `tokio::select!` in the calling application.
    ///
    /// `start_time` / `stop_time` are RFC 3339 replay bounds ([RFC5277 2.1.1](https://www.rfc-editor.org/rfc/rfc5277.html#section-2.1.1)).
    /// `stop_time` requires `start_time`.
    #[cfg(feature = "tokio")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
    pub async fn notification(
        &mut self,
        sender: Sender<String>,
        stream: Option<&str>,
        filter: Option<Filter>,
        start_time: Option<&str>,
        stop_time: Option<&str>,
    ) -> NetconfClientResult<()> {
        let start_time = start_time
            .map(crate::message::parse_date_time)
            .transpose()?;
        let stop_time = stop_time.map(crate::message::parse_date_time).transpose()?;
        let notification = Rpc::new_with_operation(RpcOperation::new_create_subscription(
            stream, filter, start_time, stop_time,
        )?);
        self.run_rpc(notification).await?;
        self.run_notification_loop(sender).await
    }

    #[cfg(feature = "tokio")]
    async fn run_notification_loop(&mut self, sender: Sender<String>) -> NetconfClientResult<()> {
        loop {
            let notification = self.transport.receive().await?;
            if sender.send(notification).await.is_err() {
                // Receiver is gone, so nothing is listening any more.
                return Ok(());
            }
        }
    }

    async fn run_rpc(&mut self, mut rpc: Rpc) -> NetconfClientResult<String> {
        let is_commit = rpc.is_commit();
        let message_id = self.allocate_message_id();
        rpc.set_message_id(&message_id);
        self.exchange(
            &rpc.to_string(),
            Some(&message_id),
            is_commit,
            self.warnings_as_errors,
        )
        .await
    }

    fn allocate_message_id(&mut self) -> String {
        let id = self.next_message_id;
        self.next_message_id += 1;
        id.to_string()
    }

    async fn exchange(
        &mut self,
        request: &str,
        expected_message_id: Option<&str>,
        is_commit: bool,
        warnings_as_errors: bool,
    ) -> NetconfClientResult<String> {
        if self.desynchronized {
            return Err(NetconfClientError::SessionDesynchronized);
        }
        let response = self.write_and_receive(request, is_commit).await?;
        debug!("RPC:\n{}", response);

        let reply = match from_xml::<RpcReply>(&response) {
            Ok(reply) => reply,
            Err(err) if self.parse_replies => return Err(err),
            Err(_) => return Ok(response),
        };

        if let Some(expected) = expected_message_id
            && reply.message_id() != Some(expected)
        {
            // The mismatched message is already consumed; the real reply may
            // still arrive and would be read as the next answer.
            self.desynchronized = true;
            return Err(NetconfClientError::MessageIdMismatch {
                expected: expected.to_string(),
                actual: reply.message_id().unwrap_or("<none>").to_string(),
            });
        }

        if self.parse_replies && reply_is_failure(&reply, warnings_as_errors) {
            return Err(NetconfClientError::Netconf(reply));
        }
        Ok(response)
    }

    #[cfg(feature = "tokio")]
    async fn write_and_receive(
        &mut self,
        request: &str,
        is_commit: bool,
    ) -> NetconfClientResult<String> {
        let result = if let Some(timeout) = self.timeout {
            match tokio::time::timeout(timeout, self.transport.write_and_receive(request)).await {
                Ok(result) => result,
                Err(_) => {
                    // The reply may still land and would be read as the answer to
                    // whatever is sent next.
                    self.desynchronized = true;
                    return Err(NetconfClientError::Timeout { timeout });
                }
            }
        } else {
            self.transport.write_and_receive(request).await
        };
        map_commit_eof(result, is_commit)
    }

    #[cfg(not(feature = "tokio"))]
    async fn write_and_receive(
        &mut self,
        request: &str,
        is_commit: bool,
    ) -> NetconfClientResult<String> {
        map_commit_eof(self.transport.write_and_receive(request).await, is_commit)
    }
}

fn reply_is_failure(reply: &RpcReply, warnings_as_errors: bool) -> bool {
    reply.has_errors() || (warnings_as_errors && reply.has_warnings())
}

fn map_commit_eof(
    result: NetconfClientResult<String>,
    is_commit: bool,
) -> NetconfClientResult<String> {
    match result {
        Err(err) if is_commit && err.is_unexpected_eof() => Err(NetconfClientError::CommitUnknown),
        other => other,
    }
}

fn request_message_id(xml: &str) -> Option<String> {
    for key in ["message-id=\"", "message-id='"] {
        if let Some(start) = xml.find(key) {
            let rest = &xml[start + key.len()..];
            let quote = key.chars().next_back()?;
            if let Some(end) = rest.find(quote) {
                let id = &rest[..end];
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

/// True when the document contains a `<commit>` or `<commit-configuration>`.
///
/// `<cancel-commit>` and `<commit-id>` do not match: their local names differ.
fn request_is_commit(xml: &str) -> bool {
    let mut rest = xml;
    while let Some(idx) = rest.find('<') {
        rest = &rest[idx + 1..];
        if rest.starts_with('!') || rest.starts_with('?') || rest.starts_with('/') {
            continue;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == "commit" || local == "commit-configuration" {
            return true;
        }
    }
    false
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.is_closed {
            warn!("NETCONF session dropped without close_session; the device may hold it open");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{request_is_commit, request_message_id};
    use crate::message::capability_id;
    use crate::{CANDIDATE_CAP, URL_CAP};

    #[test]
    fn capability_id_strips_query() {
        assert_eq!(capability_id(CANDIDATE_CAP), CANDIDATE_CAP);
        assert_eq!(
            capability_id(&format!("{URL_CAP}?scheme=http,ftp,file")),
            URL_CAP
        );
    }

    #[test]
    fn request_message_id_reads_double_and_single_quotes() {
        assert_eq!(
            request_message_id(r#"<rpc message-id="abc-123"><get/></rpc>"#).as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            request_message_id("<rpc message-id='x'><get/></rpc>").as_deref(),
            Some("x")
        );
        assert_eq!(request_message_id("<rpc><get/></rpc>"), None);
    }

    #[test]
    fn request_is_commit_matches_commit_and_junos_not_cancel() {
        assert!(request_is_commit("<rpc><commit/></rpc>"));
        assert!(request_is_commit(
            r#"<rpc><nc:commit xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0"/></rpc>"#
        ));
        assert!(request_is_commit("<rpc><commit-configuration/></rpc>"));
        assert!(request_is_commit("<rpc><commit confirmed/></rpc>"));
        assert!(!request_is_commit("<rpc><cancel-commit/></rpc>"));
        assert!(!request_is_commit(
            "<rpc><get><commit-id>1</commit-id></get></rpc>"
        ));
        assert!(!request_is_commit("<rpc><get/></rpc>"));
    }
}
