use std::str::FromStr;
use std::sync::Arc;

use netconf_async::message::{CopySource, Datastore, Filter, WithDefaultsValue};
use netconf_async::{Connection, NETCONF_URN};
#[cfg(feature = "stdio")]
use rmcp::ServiceExt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use uuid::Uuid;

use crate::config::{McpConfig, McpTransport};
use crate::connect::DeviceConnect;
use crate::edit::execute_edit_config;
use crate::notification::Subscriptions;
use crate::types::{
    CommitArgs, CopyConfigArgs, EditConfigArgs, GetArgs, GetConfigArgs, GetSchemaArgs,
    NotificationCancelArgs, NotificationPullArgs, OkReply, PullReply, RpcArgs, SchemaInfo,
    SchemaReply, SchemasReply, SubscribeArgs, SubscribeReply, XmlReply,
};
use crate::{McpServeError, mcp_err, mcp_params};

const GET_SCHEMAS_FILTER: &str = concat!(
    r#"<netconf-state xmlns="urn:ietf:params:xml:ns:yang:ietf-netconf-monitoring">"#,
    "<schemas/></netconf-state>"
);

/// MCP tools backed by [`DeviceConnect`].
pub struct NetconfServer<C: DeviceConnect> {
    connector: Arc<C>,
    config: Arc<McpConfig>,
    subscriptions: Arc<Subscriptions>,
    tool_router: ToolRouter<Self>,
}

impl<C: DeviceConnect> Clone for NetconfServer<C> {
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
            config: self.config.clone(),
            subscriptions: self.subscriptions.clone(),
            tool_router: self.tool_router.clone(),
        }
    }
}

impl<C: DeviceConnect> NetconfServer<C> {
    fn new(config: McpConfig, connector: C) -> Self {
        let mut tool_router = Self::tool_router();
        if config.read_only {
            for name in ["rpc", "edit_config", "copy_config", "commit"] {
                tool_router.disable_route(name);
            }
        }
        Self {
            connector: Arc::new(connector),
            config: Arc::new(config),
            subscriptions: Arc::new(Subscriptions::default()),
            tool_router,
        }
    }

    fn check_host(&self, host: &str) -> Result<(), ErrorData> {
        self.config.allowed_subnets.check(host).map_err(mcp_params)
    }

    async fn open(&self, args: &crate::types::ConnectionArgs) -> Result<Connection, ErrorData> {
        self.subscriptions.sweep().await;
        if crate::host::destination_host(&args.host)
            .parse::<std::net::IpAddr>()
            .is_ok()
        {
            self.check_host(&args.host)?;
        }
        let mut params = args.connect_params();
        params.allowed_subnets = self.config.allowed_subnets.clone();
        self.connector.connect(params).await.map_err(mcp_err)
    }

    async fn with_connection<T>(
        &self,
        args: &crate::types::ConnectionArgs,
        f: impl AsyncFnOnce(&mut Connection) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut conn = self.open(args).await?;
        let result = f(&mut conn).await;
        let _ = conn.close_session().await;
        result
    }
}

#[tool_router]
impl<C: DeviceConnect> NetconfServer<C> {
    #[tool(
        name = "get",
        description = "Execute NETCONF <get> for operational / state data. For configuration use get_config. filter is subtree XML without a <filter> wrapper. Set output_file to write a large reply to disk instead of returning it inline.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<Json<XmlReply>, ErrorData> {
        let filter = subtree(args.filter.as_deref());
        let defaults = parse_defaults(args.with_defaults.as_deref())?;
        let output_file = args.output.output_file.clone();
        self.with_connection(&args.connection, async move |conn| {
            let reply = conn.get(filter, defaults).await.map_err(mcp_err)?;
            Ok(Json(xml_reply(reply, output_file.as_deref())?))
        })
        .await
    }

    #[tool(
        name = "get_config",
        description = "Execute NETCONF <get-config>. source is running (default), candidate, or startup. filter is subtree XML without a <filter> wrapper. Set output_file to write a large reply to disk instead of returning it inline.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn get_config(
        &self,
        Parameters(args): Parameters<GetConfigArgs>,
    ) -> Result<Json<XmlReply>, ErrorData> {
        let source = parse_datastore(args.source.as_deref(), Datastore::Running)?;
        let filter = subtree(args.filter.as_deref());
        let defaults = parse_defaults(args.with_defaults.as_deref())?;
        let output_file = args.output.output_file.clone();
        self.with_connection(&args.connection, async move |conn| {
            let reply = conn
                .get_config(source, filter, defaults)
                .await
                .map_err(mcp_err)?;
            Ok(Json(xml_reply(reply, output_file.as_deref())?))
        })
        .await
    }

    #[tool(
        name = "rpc",
        description = "Execute a custom NETCONF RPC. rpc is the body without an <rpc> wrapper (a full <rpc> document is also accepted). Set output_file to write a large reply to disk instead of returning it inline.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn rpc(
        &self,
        Parameters(args): Parameters<RpcArgs>,
    ) -> Result<Json<XmlReply>, ErrorData> {
        let xml = wrap_rpc(&args.rpc);
        let output_file = args.output.output_file.clone();
        self.with_connection(&args.connection, async move |conn| {
            let reply = conn.raw_rpc(&xml).await.map_err(mcp_err)?;
            Ok(Json(xml_reply(reply, output_file.as_deref())?))
        })
        .await
    }

    #[tool(
        name = "edit_config",
        description = "Modify configuration: lock, edit-config, validate, commit, unlock. copy_to_startup defaults to false. confirmed=true returns persist_id for the commit tool.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn edit_config(
        &self,
        Parameters(args): Parameters<EditConfigArgs>,
    ) -> Result<Json<OkReply>, ErrorData> {
        let connection = args.connection.clone();
        self.with_connection(&connection, async move |conn| {
            let outcome = execute_edit_config(conn, &args).await.map_err(mcp_err)?;
            Ok(Json(OkReply {
                success: true,
                persist_id: outcome.persist_id,
                message: outcome.warning,
            }))
        })
        .await
    }

    #[tool(
        name = "copy_config",
        description = "Copy configuration between datastores or URLs. Example: source=running target=startup.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn copy_config(
        &self,
        Parameters(args): Parameters<CopyConfigArgs>,
    ) -> Result<Json<OkReply>, ErrorData> {
        let source = CopySource::from(Datastore::from_str(&args.source).map_err(mcp_params)?);
        let target = Datastore::from_str(&args.target).map_err(mcp_params)?;
        self.with_connection(&args.connection, async move |conn| {
            conn.copy_config(source, target).await.map_err(mcp_err)?;
            Ok(Json(OkReply {
                success: true,
                persist_id: None,
                message: None,
            }))
        })
        .await
    }

    #[tool(
        name = "commit",
        description = "Confirm or cancel a persist confirmed-commit from edit_config(confirmed=true).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn commit(
        &self,
        Parameters(args): Parameters<CommitArgs>,
    ) -> Result<Json<OkReply>, ErrorData> {
        let connection = args.connection.clone();
        self.with_connection(&connection, async move |conn| {
            if args.cancel {
                conn.cancel_commit(Some(args.persist_id.clone()))
                    .await
                    .map_err(mcp_err)?;
                Ok(Json(OkReply {
                    success: true,
                    persist_id: None,
                    message: Some("cancel-commit completed".into()),
                }))
            } else {
                conn.confirm_commit(args.persist_id.clone())
                    .await
                    .map_err(mcp_err)?;
                Ok(Json(OkReply {
                    success: true,
                    persist_id: None,
                    message: Some("commit completed".into()),
                }))
            }
        })
        .await
    }

    #[tool(
        name = "list_schemas",
        description = "List YANG schemas from ietf-netconf-monitoring. Use get_schema to fetch one.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn list_schemas(
        &self,
        Parameters(args): Parameters<crate::types::ConnectionArgs>,
    ) -> Result<Json<SchemasReply>, ErrorData> {
        self.with_connection(&args, async move |conn| {
            let reply = conn
                .get(Some(Filter::subtree(GET_SCHEMAS_FILTER)), None)
                .await
                .map_err(mcp_err)?;
            Ok(Json(SchemasReply {
                success: true,
                schemas: parse_schemas(&reply),
            }))
        })
        .await
    }

    #[tool(
        name = "get_schema",
        description = "Fetch one YANG schema by identifier (from list_schemas). Set output_file to write the schema to disk instead of returning it inline.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn get_schema(
        &self,
        Parameters(args): Parameters<GetSchemaArgs>,
    ) -> Result<Json<SchemaReply>, ErrorData> {
        let identifier = args.identifier.clone();
        let version = args.version.clone();
        let format = args.format.clone();
        let output_file = args.output.output_file.clone();
        self.with_connection(&args.connection, async move |conn| {
            let reply = conn
                .get_schema(&identifier, version.as_deref(), format.as_deref())
                .await
                .map_err(mcp_err)?;
            Ok(Json(schema_reply(
                extract_schema_body(&reply),
                output_file.as_deref(),
            )?))
        })
        .await
    }

    #[tool(
        name = "subscribe",
        description = "Create a listen-only notification subscription. Other tools still open their own sessions. Pull with notification_pull, stop with notification_cancel.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn subscribe(
        &self,
        Parameters(args): Parameters<SubscribeArgs>,
    ) -> Result<Json<SubscribeReply>, ErrorData> {
        let mut conn = self.open(&args.connection).await?;
        let stream = args.stream.as_deref().unwrap_or("NETCONF");
        let filter = subtree(args.filter.as_deref());
        if let Err(err) = conn
            .create_subscription(
                Some(stream),
                filter,
                args.start_time.as_deref(),
                args.stop_time.as_deref(),
            )
            .await
        {
            let _ = conn.close_session().await;
            return Err(mcp_err(err));
        }
        let subscription_id = self.subscriptions.insert(conn).await?;
        Ok(Json(SubscribeReply {
            success: true,
            subscription_id,
        }))
    }

    #[tool(
        name = "notification_pull",
        description = "Drain notifications for a subscription_id. wait_ms=0 returns immediately; otherwise wait up to that many milliseconds for at least one.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = true)
    )]
    async fn notification_pull(
        &self,
        Parameters(args): Parameters<NotificationPullArgs>,
    ) -> Result<Json<PullReply>, ErrorData> {
        let notifications = self
            .subscriptions
            .pull(&args.subscription_id, args.wait_ms)
            .await?;
        Ok(Json(PullReply {
            success: true,
            notifications,
        }))
    }

    #[tool(
        name = "notification_cancel",
        description = "Close a notification subscription and its NETCONF session.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn notification_cancel(
        &self,
        Parameters(args): Parameters<NotificationCancelArgs>,
    ) -> Result<Json<OkReply>, ErrorData> {
        self.subscriptions.cancel(&args.subscription_id).await?;
        Ok(Json(OkReply {
            success: true,
            persist_id: None,
            message: Some("subscription cancelled".into()),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl<C: DeviceConnect> ServerHandler for NetconfServer<C> {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("netconf-mcp", self.config.version.clone())
                    .with_description("NETCONF tools for network devices"),
            )
            .with_instructions(
                "NETCONF client. host is required on each tool (optional :port, default 830). Password is optional; prefer NETCONF_USERNAME / NETCONF_PASSWORD or ~/.ssh/config. edit_config locks, validates, and commits. Notifications: subscribe, then notification_pull / notification_cancel.",
            )
    }
}

pub(crate) async fn run<C: DeviceConnect>(
    config: McpConfig,
    connector: C,
) -> Result<(), McpServeError> {
    match config.transport.clone() {
        #[cfg(feature = "stdio")]
        McpTransport::Stdio => run_stdio(config, connector).await,
        #[cfg(feature = "http")]
        McpTransport::Http { bind } => run_http(config, connector, bind).await,
    }
}

#[cfg(feature = "stdio")]
async fn run_stdio<C: DeviceConnect>(config: McpConfig, connector: C) -> Result<(), McpServeError> {
    use rmcp::transport::stdio;
    log::info!("Starting MCP server in stdio mode");
    if config.read_only {
        log::info!("Read-only mode enabled");
    }
    let server = NetconfServer::new(config, connector);
    let running = server
        .serve(stdio())
        .await
        .map_err(|err| McpServeError::Serve(err.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|err| McpServeError::Serve(err.to_string()))?;
    Ok(())
}

#[cfg(feature = "http")]
async fn run_http<C: DeviceConnect>(
    config: McpConfig,
    connector: C,
    bind: std::net::SocketAddr,
) -> Result<(), McpServeError> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    use tokio_util::sync::CancellationToken;

    if !bind.ip().is_loopback() {
        return Err(McpServeError::Serve(format!(
            "HTTP bind {bind} is not loopback; refusing non-localhost listen"
        )));
    }
    log::info!("Starting MCP server in HTTP mode on {bind}");
    if config.read_only {
        log::info!("Read-only mode enabled");
    }
    let ct = CancellationToken::new();
    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        }
    });
    let connector = Arc::new(connector);
    let service: StreamableHttpService<NetconfServer<ArcConnector<C>>, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let config = config.clone();
                let connector = connector.clone();
                move || {
                    Ok(NetconfServer::new(
                        config.clone(),
                        ArcConnector(connector.clone()),
                    ))
                }
            },
            Default::default(),
            StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled_owned().await })
        .await?;
    Ok(())
}

/// [`DeviceConnect`] wrapper so the HTTP factory can clone an `Arc<C>`.
#[cfg(feature = "http")]
struct ArcConnector<C>(Arc<C>);

#[cfg(feature = "http")]
#[async_trait::async_trait]
impl<C: DeviceConnect> DeviceConnect for ArcConnector<C> {
    async fn connect(
        &self,
        params: crate::ConnectParams,
    ) -> netconf_async::NetconfClientResult<Connection> {
        self.0.connect(params).await
    }
}

fn subtree(filter: Option<&str>) -> Option<Filter> {
    filter
        .map(str::trim)
        .filter(|xml| !xml.is_empty())
        .map(Filter::subtree)
}

fn parse_defaults(value: Option<&str>) -> Result<Option<WithDefaultsValue>, ErrorData> {
    match value {
        None | Some("") => Ok(None),
        Some(value) => WithDefaultsValue::from_str(value)
            .map(Some)
            .map_err(mcp_params),
    }
}

fn parse_datastore(value: Option<&str>, default: Datastore) -> Result<Datastore, ErrorData> {
    match value {
        None | Some("") => Ok(default),
        Some(value) => Datastore::from_str(value).map_err(mcp_params),
    }
}

pub(crate) fn wrap_rpc(xml: &str) -> String {
    let body = strip_xml_decl(xml.trim());
    if is_rpc_document(body) {
        return body.to_string();
    }
    format!(
        r#"<rpc message-id="{}" xmlns="{NETCONF_URN}">{body}</rpc>"#,
        Uuid::new_v4()
    )
}

fn is_rpc_document(xml: &str) -> bool {
    let Some(rest) = xml.trim_start().strip_prefix('<') else {
        return false;
    };
    let name = rest
        .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .next()
        .unwrap_or("");
    let local = name
        .rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name);
    local == "rpc"
}

fn strip_xml_decl(xml: &str) -> &str {
    let xml = xml.trim_start();
    if let Some(rest) = xml.strip_prefix("<?")
        && let Some(end) = rest.find("?>")
    {
        return rest[end + 2..].trim_start();
    }
    xml
}

fn xml_reply(xml: String, output_file: Option<&str>) -> Result<XmlReply, ErrorData> {
    let saved = save_xml(xml, output_file)?;
    Ok(XmlReply {
        success: true,
        reply: saved.inline,
        file: saved.file,
        bytes: saved.bytes,
    })
}

fn schema_reply(xml: String, output_file: Option<&str>) -> Result<SchemaReply, ErrorData> {
    let saved = save_xml(xml, output_file)?;
    Ok(SchemaReply {
        success: true,
        schema: saved.inline,
        file: saved.file,
        bytes: saved.bytes,
    })
}

struct SavedXml {
    inline: Option<String>,
    file: Option<String>,
    bytes: Option<u64>,
}

fn save_xml(xml: String, output_file: Option<&str>) -> Result<SavedXml, ErrorData> {
    let Some(path) = output_file.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(SavedXml {
            inline: Some(xml),
            file: None,
            bytes: None,
        });
    };
    let path = expand_tilde(path);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| mcp_params(format!("failed to create {}: {err}", parent.display())))?;
    }
    std::fs::write(&path, xml.as_bytes())
        .map_err(|err| mcp_params(format!("failed to write {}: {err}", path.display())))?;
    let bytes = xml.len() as u64;
    let file = std::fs::canonicalize(&path)
        .unwrap_or(path)
        .display()
        .to_string();
    Ok(SavedXml {
        inline: None,
        file: Some(file),
        bytes: Some(bytes),
    })
}

fn expand_tilde(path: &str) -> std::path::PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| std::path::PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(mut home) = home_dir()
    {
        home.push(rest);
        return home;
    }
    std::path::PathBuf::from(path)
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

pub(crate) fn parse_schemas(xml: &str) -> Vec<SchemaInfo> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut schemas = Vec::new();
    let mut current: Option<SchemaInfo> = None;
    let mut field = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match local_name(event.name().as_ref()) {
                "schema" => {
                    current = Some(SchemaInfo {
                        identifier: String::new(),
                        version: String::new(),
                        format: String::new(),
                        namespace: String::new(),
                    });
                    field = None;
                }
                name if current.is_some() => field = schema_field(name),
                _ => field = None,
            },
            Ok(Event::Text(text)) => {
                if let (Some(schema), Some(field)) = (current.as_mut(), field)
                    && let Ok(value) = text.xml10_content()
                {
                    set_schema_field(schema, field, value.as_ref());
                }
            }
            Ok(Event::CData(text)) => {
                if let (Some(schema), Some(field)) = (current.as_mut(), field)
                    && let Ok(value) = text.decode()
                {
                    set_schema_field(schema, field, value.as_ref());
                }
            }
            Ok(Event::End(event)) => {
                if local_name(event.name().as_ref()) == "schema"
                    && let Some(schema) = current.take()
                {
                    schemas.push(schema);
                }
                field = None;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    schemas
}

fn schema_field(name: &str) -> Option<&'static str> {
    match name {
        "identifier" => Some("identifier"),
        "version" => Some("version"),
        "format" => Some("format"),
        "namespace" => Some("namespace"),
        _ => None,
    }
}

fn set_schema_field(schema: &mut SchemaInfo, field: &str, value: &str) {
    let slot = match field {
        "identifier" => &mut schema.identifier,
        "version" => &mut schema.version,
        "format" => &mut schema.format,
        "namespace" => &mut schema.namespace,
        _ => return,
    };
    slot.push_str(value);
}

fn local_name(name: &[u8]) -> &str {
    let name = std::str::from_utf8(name).unwrap_or("");
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

pub(crate) fn extract_schema_body(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut depth = 0u32;
    let mut body = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) if local_name(event.name().as_ref()) == "data" => {
                depth = 1;
            }
            Ok(Event::Start(_)) if depth > 0 => depth += 1,
            Ok(Event::End(event)) => {
                if local_name(event.name().as_ref()) == "data" && depth == 1 {
                    break;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(text)) if depth == 1 => {
                if let Ok(value) = text.xml10_content() {
                    body.push_str(value.as_ref());
                }
            }
            Ok(Event::CData(text)) if depth == 1 => {
                if let Ok(value) = text.decode() {
                    body.push_str(value.as_ref());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if body.is_empty() {
        xml.to_string()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_rpc_leaves_full_document() {
        let xml = r#"<rpc message-id="1" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><discard-changes/></rpc>"#;
        assert_eq!(wrap_rpc(xml), xml);
    }

    #[test]
    fn wrap_rpc_wraps_inner_body() {
        let wrapped = wrap_rpc("<discard-changes/>");
        assert!(wrapped.contains("<discard-changes/>"), "{wrapped}");
        assert!(wrapped.starts_with("<rpc "), "{wrapped}");
    }

    #[test]
    fn wrap_rpc_strips_decl_before_wrap() {
        let wrapped = wrap_rpc(r#"<?xml version="1.0"?><discard-changes/>"#);
        assert!(!wrapped.contains("?xml"), "{wrapped}");
        assert!(wrapped.contains("<discard-changes/>"), "{wrapped}");
        assert!(wrapped.starts_with("<rpc "), "{wrapped}");
    }

    #[test]
    fn wrap_rpc_detects_prefixed_rpc_root() {
        let xml = r#"<nc:rpc xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0" message-id="1"><nc:get/></nc:rpc>"#;
        assert_eq!(wrap_rpc(xml), xml);
        let with_decl = format!(r#"<?xml version="1.0"?>{xml}"#);
        assert_eq!(wrap_rpc(&with_decl), xml);
    }

    #[test]
    fn parse_schemas_reads_entries() {
        let xml = r#"<rpc-reply><data>
            <netconf-state xmlns="urn:ietf:params:xml:ns:yang:ietf-netconf-monitoring">
              <schemas>
                <schema>
                  <identifier>ietf-interfaces</identifier>
                  <version>2018-02-20</version>
                  <format>yang</format>
                  <namespace>urn:ietf:params:xml:ns:yang:ietf-interfaces</namespace>
                </schema>
                <nc:schema xmlns:nc="urn:ietf:params:xml:ns:yang:ietf-netconf-monitoring">
                  <nc:identifier>nokia-conf</nc:identifier>
                  <nc:version>1</nc:version>
                  <nc:format>yang</nc:format>
                  <nc:namespace>urn:nokia</nc:namespace>
                </nc:schema>
              </schemas>
            </netconf-state>
        </data></rpc-reply>"#;
        let schemas = parse_schemas(xml);
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0].identifier, "ietf-interfaces");
        assert_eq!(schemas[1].identifier, "nokia-conf");
        assert_eq!(
            schemas[0].namespace,
            "urn:ietf:params:xml:ns:yang:ietf-interfaces"
        );
    }

    #[test]
    fn extract_schema_strips_cdata() {
        let xml = "<rpc-reply><data><![CDATA[module foo {}]]></data></rpc-reply>";
        assert_eq!(extract_schema_body(xml), "module foo {}");
    }

    #[test]
    fn save_xml_writes_and_omits_inline() {
        let dir = std::env::temp_dir().join(format!("netconf-mcp-{}", Uuid::new_v4()));
        let path = dir.join("nested").join("reply.xml");
        let xml = "<rpc-reply><ok/></rpc-reply>";
        let saved = save_xml(xml.into(), Some(path.to_str().unwrap())).unwrap();
        assert!(saved.inline.is_none());
        assert_eq!(saved.bytes, Some(xml.len() as u64));
        let file = saved.file.expect("path");
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written, xml);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_xml_without_path_keeps_inline() {
        let saved = save_xml("<ok/>".into(), None).unwrap();
        assert_eq!(saved.inline.as_deref(), Some("<ok/>"));
        assert!(saved.file.is_none());
        let saved = save_xml("<ok/>".into(), Some("  ")).unwrap();
        assert_eq!(saved.inline.as_deref(), Some("<ok/>"));
    }

    struct NoopConnect;

    #[async_trait::async_trait]
    impl DeviceConnect for NoopConnect {
        async fn connect(
            &self,
            _params: crate::ConnectParams,
        ) -> netconf_async::NetconfClientResult<Connection> {
            Err(netconf_async::NetconfClientError::new("noop"))
        }
    }

    fn annotations(name: &str) -> rmcp::model::ToolAnnotations {
        NetconfServer::<NoopConnect>::tool_router()
            .get(name)
            .unwrap_or_else(|| panic!("missing tool {name}"))
            .annotations
            .clone()
            .unwrap_or_else(|| panic!("missing annotations on {name}"))
    }

    #[test]
    fn read_tools_are_marked_read_only() {
        for name in [
            "get",
            "get_config",
            "list_schemas",
            "get_schema",
            "subscribe",
            "notification_pull",
            "notification_cancel",
        ] {
            let ann = annotations(name);
            assert_eq!(ann.read_only_hint, Some(true), "{name}");
            assert_eq!(ann.open_world_hint, Some(true), "{name}");
        }
        let edit = annotations("edit_config");
        assert_eq!(edit.read_only_hint, Some(false));
        assert_eq!(edit.destructive_hint, Some(true));
        assert_eq!(edit.idempotent_hint, Some(false));
        assert_eq!(annotations("rpc").destructive_hint, Some(true));
        assert_eq!(annotations("subscribe").idempotent_hint, Some(false));
    }
}
