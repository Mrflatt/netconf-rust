#![allow(dead_code)]
use crate::{NETCONF_URN, error};
use core::fmt;
use core::fmt::Display;
use core::ops::Add;
use core::str::FromStr;
use core::time::Duration;
use quick_xml::escape::unescape;
use quick_xml::se::Serializer;
use serde_derive::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename(serialize = "hello"))]
pub struct Hello {
    #[serde(rename = "@xmlns")]
    xmlns: String,
    capabilities: Capabilities,
    #[serde(rename = "session-id", skip_serializing_if = "Option::is_none")]
    session_id: Option<u64>,
}

impl Hello {
    pub fn new() -> Hello {
        Hello {
            xmlns: NETCONF_URN.to_string(),
            session_id: None,
            capabilities: Capabilities {
                capability: vec![
                    "urn:ietf:params:netconf:base:1.0".to_string(),
                    "urn:ietf:params:netconf:base:1.1".to_string(),
                ],
            },
        }
    }

    pub fn capabilities(&self) -> Vec<String> {
        self.capabilities
            .capability
            .iter()
            .map(|capability| capability.to_string())
            .collect()
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities
            .capability
            .iter()
            .any(|cap| cap == capability)
    }

    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }
}

impl Display for Hello {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use serde::Serialize;
        let mut buffer = String::with_capacity(206);
        let ser = Serializer::new(&mut buffer);
        self.serialize(ser).unwrap();
        write!(f, "{}", buffer)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Capabilities {
    capability: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Rpc {
    #[serde(rename = "@message-id")]
    message_id: String,
    #[serde(rename = "@xmlns")]
    xmlns: String,
    #[serde(rename = "$value")]
    operation: RpcOperation,
}

impl Rpc {
    pub fn new_with_operation(operation: RpcOperation) -> Rpc {
        Rpc {
            xmlns: NETCONF_URN.to_string(),
            message_id: Uuid::new_v4().to_string(),
            operation,
        }
    }
}

impl Display for Rpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use serde::Serialize;
        let mut buffer = String::with_capacity(256);
        let mut ser = Serializer::with_root(&mut buffer, Some("rpc")).unwrap();
        ser.indent(' ', 2);
        self.serialize(ser).unwrap();
        match &self.operation {
            RpcOperation::GetConfig { .. }
            | RpcOperation::Get { .. }
            | RpcOperation::EditConfig { .. }
            | RpcOperation::CopyConfig { .. } => {
                write!(f, "{}", unescape(buffer.as_str()).unwrap())
            }
            _ => {
                write!(f, "{}", buffer)
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RpcOperation {
    CloseSession,
    KillSession {
        #[serde(rename = "session-id")]
        session_id: u64,
    },
    Validate {
        source: Source,
    },
    GetConfig(GetConfig),
    Get(Get),
    EditConfig(EditConfig),
    CopyConfig(CopyConfig),
    DeleteConfig {
        target: Target,
    },
    Lock {
        target: Target,
    },
    Unlock {
        target: Target,
    },
    DiscardChanges,
    CancelCommit(CancelCommit),
    Commit(Commit),
    CreateSubscription(CreateSubscription),
}

impl RpcOperation {
    pub fn new_get_config(
        datastore: Datastore,
        filter: Option<Filter>,
        defaults: Option<WithDefaultsValue>,
    ) -> RpcOperation {
        RpcOperation::GetConfig(GetConfig {
            source: Source { datastore },
            filter,
            with_defaults: defaults.map(|value| WithDefaults {
                xmlns: "urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults".to_string(),
                value,
            }),
        })
    }

    pub fn new_get(filter: Option<Filter>, defaults: Option<WithDefaultsValue>) -> RpcOperation {
        RpcOperation::Get(Get {
            filter,
            with_defaults: defaults.map(|value| WithDefaults {
                xmlns: "urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults".to_string(),
                value,
            }),
        })
    }

    pub fn new_commit(
        confirmed: Option<()>,
        confirm_timeout: Option<i32>,
        persist: Option<String>,
        persist_id: Option<String>,
    ) -> RpcOperation {
        RpcOperation::Commit(Commit {
            confirmed,
            confirm_timeout,
            persist,
            persist_id,
        })
    }

    pub fn new_create_subscription(
        stream: Option<&str>,
        filter: Option<Filter>,
        duration: Option<Duration>,
    ) -> RpcOperation {
        let (start_time, stop_time) = if let Some(duration) = duration {
            let now = OffsetDateTime::now_utc();
            (Some(OffsetDateTime::now_utc()), Some(now.add(duration)))
        } else {
            (None, None)
        };
        RpcOperation::CreateSubscription(CreateSubscription {
            xmlns: "urn:ietf:params:xml:ns:netconf:notification:1.0".to_string(),
            stream: stream.map(|s| s.to_string()),
            filter,
            start_time,
            stop_time,
        })
    }

    pub fn new_edit_config(
        target: Datastore,
        content: EditContent,
        default_operation: Option<DefaultOperation>,
        test_option: Option<TestOption>,
        error_option: Option<ErrorOption>,
    ) -> RpcOperation {
        let (config, url) = match content {
            EditContent::Config(xml) => (Some(InlineConfig::new(&xml)), None),
            EditContent::Url(url) => (None, Some(url)),
        };
        RpcOperation::EditConfig(EditConfig {
            target: Target { datastore: target },
            default_operation,
            test_option,
            error_option,
            config,
            url,
        })
    }

    pub fn new_copy_config(source: CopySource, target: Datastore) -> RpcOperation {
        let source = match source {
            CopySource::Config(xml) => CopySource::Config(strip_config_wrapper(&xml).to_string()),
            other => other,
        };
        RpcOperation::CopyConfig(CopyConfig {
            target: Target { datastore: target },
            source: CopySourceXml { value: source },
        })
    }

    pub fn new_delete_config(target: Datastore) -> RpcOperation {
        RpcOperation::DeleteConfig {
            target: Target { datastore: target },
        }
    }

    pub fn new_lock(target: Datastore) -> RpcOperation {
        RpcOperation::Lock {
            target: Target { datastore: target },
        }
    }

    pub fn new_unlock(target: Datastore) -> RpcOperation {
        RpcOperation::Unlock {
            target: Target { datastore: target },
        }
    }

    pub fn new_discard_changes() -> RpcOperation {
        RpcOperation::DiscardChanges
    }

    pub fn new_cancel_commit(persist_id: Option<String>) -> RpcOperation {
        RpcOperation::CancelCommit(CancelCommit { persist_id })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Commit {
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmed: Option<()>,
    #[serde(rename = "confirm-timeout", skip_serializing_if = "Option::is_none")]
    confirm_timeout: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persist: Option<String>,
    #[serde(rename = "persist-id", skip_serializing_if = "Option::is_none")]
    persist_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Get {
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    with_defaults: Option<WithDefaults>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct GetConfig {
    source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    with_defaults: Option<WithDefaults>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WithDefaults {
    #[serde(rename = "@xmlns")]
    xmlns: String,
    #[serde(rename = "$text")]
    value: WithDefaultsValue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WithDefaultsValue {
    ReportAll,
    ReportAllTagged,
    Trim,
    Explicit,
}

impl FromStr for WithDefaultsValue {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let defaults = s.to_lowercase();
        match defaults.as_str() {
            "report-all" => Ok(WithDefaultsValue::ReportAll),
            "report-all-tagged" => Ok(WithDefaultsValue::ReportAllTagged),
            "trim" => Ok(WithDefaultsValue::Trim),
            "explicit" => Ok(WithDefaultsValue::Explicit),
            _ => Err(error::NetconfClientError::new(format!(
                "unknown with-defaults value: {}",
                s
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Source {
    #[serde(rename = "$value")]
    pub datastore: Datastore,
}

/// Destination datastore of `<edit-config>` / `<copy-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Serialize)]
pub struct Target {
    #[serde(rename = "$value")]
    pub datastore: Datastore,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Datastore {
    Candidate,
    Running,
    Startup,
    Url(String),
}

impl FromStr for Datastore {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let datastore = s.to_lowercase();
        match datastore.as_str() {
            "running" => Ok(Datastore::Running),
            "candidate" => Ok(Datastore::Candidate),
            "startup" => Ok(Datastore::Startup),
            _ => {
                if datastore.starts_with("http")
                    || datastore.starts_with("file")
                    || datastore.starts_with("ftp")
                {
                    Ok(Datastore::Url(datastore))
                } else {
                    Err(error::NetconfClientError::UnknownDatastore {
                        expected: vec![
                            "running".to_string(),
                            "candidate".to_string(),
                            "startup".to_string(),
                            "ftp|http|file".to_string(),
                        ],
                        unknown: datastore,
                    })
                }
            }
        }
    }
}

/// Payload of `<edit-config>`: inline config XML or a URL ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug)]
pub enum EditContent {
    Config(String),
    Url(String),
}

/// `default-operation` of `<edit-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Clone, Copy)]
pub enum DefaultOperation {
    Merge,
    Replace,
    None,
}

impl DefaultOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Replace => "replace",
            Self::None => "none",
        }
    }
}

impl serde::Serialize for DefaultOperation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for DefaultOperation {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "merge" => Ok(Self::Merge),
            "replace" => Ok(Self::Replace),
            "none" => Ok(Self::None),
            _ => Err(error::NetconfClientError::new(format!(
                "unknown default-operation: {s}"
            ))),
        }
    }
}

/// `test-option` of `<edit-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Clone, Copy)]
pub enum TestOption {
    TestThenSet,
    Set,
    TestOnly,
}

impl TestOption {
    fn as_str(self) -> &'static str {
        match self {
            Self::TestThenSet => "test-then-set",
            Self::Set => "set",
            Self::TestOnly => "test-only",
        }
    }
}

impl serde::Serialize for TestOption {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for TestOption {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "test-then-set" => Ok(Self::TestThenSet),
            "set" => Ok(Self::Set),
            "test-only" => Ok(Self::TestOnly),
            _ => Err(error::NetconfClientError::new(format!(
                "unknown test-option: {s}"
            ))),
        }
    }
}

/// `error-option` of `<edit-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Clone, Copy)]
pub enum ErrorOption {
    StopOnError,
    ContinueOnError,
    RollbackOnError,
}

impl ErrorOption {
    fn as_str(self) -> &'static str {
        match self {
            Self::StopOnError => "stop-on-error",
            Self::ContinueOnError => "continue-on-error",
            Self::RollbackOnError => "rollback-on-error",
        }
    }
}

impl serde::Serialize for ErrorOption {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for ErrorOption {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stop-on-error" => Ok(Self::StopOnError),
            "continue-on-error" => Ok(Self::ContinueOnError),
            "rollback-on-error" => Ok(Self::RollbackOnError),
            _ => Err(error::NetconfClientError::new(format!(
                "unknown error-option: {s}"
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
struct InlineConfig {
    #[serde(rename = "$value")]
    xml: String,
}

impl InlineConfig {
    fn new(xml: &str) -> Self {
        Self {
            xml: strip_config_wrapper(xml).to_string(),
        }
    }
}

fn strip_config_wrapper(xml: &str) -> &str {
    let xml = xml.trim();
    let Some(after) = xml.strip_prefix("<config") else {
        return xml;
    };
    let first = after.chars().next();
    if !matches!(first, Some('>' | ' ' | '\t' | '\n' | '\r' | '/')) {
        return xml;
    }
    if !xml.ends_with("</config>") {
        return xml;
    }
    let Some(start) = xml.find('>') else {
        return xml;
    };
    let end = xml.len() - "</config>".len();
    if start + 1 >= end {
        return xml;
    }
    xml[start + 1..end].trim()
}

/// `<edit-config>` body ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct EditConfig {
    target: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_operation: Option<DefaultOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_option: Option<TestOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_option: Option<ErrorOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<InlineConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

/// Source of `<copy-config>` ([RFC6241 7.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.3)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CopySource {
    Candidate,
    Running,
    Startup,
    Url(String),
    Config(String),
}

impl From<Datastore> for CopySource {
    fn from(datastore: Datastore) -> Self {
        match datastore {
            Datastore::Candidate => Self::Candidate,
            Datastore::Running => Self::Running,
            Datastore::Startup => Self::Startup,
            Datastore::Url(url) => Self::Url(url),
        }
    }
}

#[derive(Debug, Serialize)]
struct CopySourceXml {
    #[serde(rename = "$value")]
    value: CopySource,
}

/// `<copy-config>` body ([RFC6241 7.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.3)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CopyConfig {
    target: Target,
    source: CopySourceXml,
}

/// `<cancel-commit>` body ([RFC6241 8.4.4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4.4.1)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct CancelCommit {
    #[serde(rename = "persist-id", skip_serializing_if = "Option::is_none")]
    persist_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Filter {
    #[serde(rename = "@type")]
    filter_type: String,
    #[serde(rename = "$value")]
    filter: String,
}

impl Filter {
    pub fn subtree(filter: &str) -> Filter {
        let filter = Filter::strip_slashes(filter).unwrap();
        Filter {
            filter_type: "subtree".to_string(),
            filter: filter.trim().to_string(),
        }
    }

    fn strip_slashes(s: &str) -> Option<String> {
        let mut n = String::new();
        let mut chars = s.trim().chars();

        while let Some(c) = chars.next() {
            n.push(match c {
                '\\' => chars.next()?,
                c => c,
            });
        }

        Some(n)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", rename(serialize = "rpc-reply"))]
pub struct RpcReply {
    #[serde(rename = "@message-id")]
    message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpc_error: Option<Vec<Error>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<()>,
}

impl RpcReply {
    pub fn is_ok(&self) -> bool {
        self.ok.is_some() && self.rpc_error.is_none()
    }

    pub fn has_errors(&self) -> bool {
        self.rpc_error.is_some()
    }

    pub fn get_message_id(&self) -> &str {
        &self.message_id
    }
}

impl Display for RpcReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use serde::Serialize;
        let mut buffer = String::with_capacity(512);
        let mut ser = Serializer::new(&mut buffer);
        ser.indent(' ', 2);
        self.serialize(ser).unwrap();
        write!(f, "{}", buffer)
    }
}

impl std::error::Error for RpcReply {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename = "rpc-error", rename_all = "kebab-case")]
pub struct Error {
    error_severity: ErrorSeverity,
    error_type: ErrorType,
    error_tag: ErrorTag,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_app_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_info: Option<ErrorInfo>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ErrorType {
    Transport,
    Rpc,
    Protocol,
    Application,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ErrorSeverity {
    Error,
    Warning,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ErrorTag {
    InUse,
    InvalidValue,
    TooBig,
    MissingAttribute,
    BadAttribute,
    UnknownAttribute,
    MissingElement,
    BadElement,
    UnknownElement,
    UnknownNamespace,
    AccessDenied,
    LockDenied,
    ResourceDenied,
    RollbackFailed,
    DataExists,
    DataMissing,
    OperationNotSupported,
    OperationFailed,
    PartialOperation,
    MalformedMessage,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
struct ErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    bad_element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bad_attribute: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bad_namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok_element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err_element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    noop_element: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct CreateSubscription {
    #[serde(rename = "@xmlns")]
    xmlns: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter>,
    #[serde(
        rename = "startTime",
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    start_time: Option<OffsetDateTime>,
    #[serde(
        rename = "stopTime",
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    stop_time: Option<OffsetDateTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use quick_xml::de::from_str;
    use time::Duration;
    use time::format_description::well_known::Rfc3339;

    #[test]
    fn test_deserialize_rpc_reply() {
        let reply = r#"
<rpc-reply message-id="67d83d6b-1f0b-47fb-8fdf-2cfc3fb2a371" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <rpc-error>
    <error-type>protocol</error-type>
    <error-tag>bad-element</error-tag>
    <error-severity>error</error-severity>
    <error-message>Element is not valid in the specified context.</error-message>
    <error-info>
      <bad-element>startu</bad-element>
    </error-info>
  </rpc-error>
  <rpc-error>
    <error-type>application</error-type>
    <error-tag>bad-element</error-tag>
    <error-severity>error</error-severity>
    <error-message>Element is not valid in the specified context.</error-message>
    <error-info>
      <bad-element>startu</bad-element>
    </error-info>
  </rpc-error>
</rpc-reply>
"#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(reply.rpc_error.is_some(), "<rpc-error> element not found");
        assert_eq!(reply.rpc_error.unwrap().len(), 2);

        let reply = r#"
<rpc-reply message-id="1" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <rpc-error>
    <error-type>application</error-type>
    <error-tag>unknown-element</error-tag>
    <error-severity>error</error-severity>
    <error-message>Unknown element</error-message>
    <error-info>
      <bad-element>startup</bad-element>
    </error-info>
  </rpc-error>
</rpc-reply>
"#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(reply.has_errors());

        let reply = r#"
<rpc-reply message-id="c60e637d-0f79-41ea-ad09-a5ee02f08434">
  <data>
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf" xmlns:nokia-attr="urn:nokia.com:sros:ns:yang:sr:attributes">
      <port>
        <port-id>1/1/2</port-id>
      </port>
      <port>
        <port-id>1/1/3</port-id>
      </port>
      <system>
        <time>
          <ntp>
            <admin-state>enable</admin-state>
            <server>
              <router-instance>Base</router-instance>
            </server>
          </ntp>
          <zone>
            <standard>
              <name>eet</name>
            </standard>
          </zone>
        </time>
      </system>
    </configure>
  </data>
</rpc-reply>
        "#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(reply.rpc_error.is_none());
        assert!(reply.ok.is_none());

        let reply = r#"
<?xml version="1.0" encoding="UTF-8"?>
<rpc-reply message-id="938f1c28-e6e3-4641-a4d0-383d9ef1a280" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <ok/>
</rpc-reply>
"#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(reply.ok.is_some());
    }

    #[test]
    fn test_serialize_hello() {
        let expected = r#"<hello xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><capabilities><capability>urn:ietf:params:netconf:base:1.0</capability><capability>urn:ietf:params:netconf:base:1.1</capability></capabilities></hello>"#;
        let hello = Hello {
            xmlns: NETCONF_URN.to_string(),
            session_id: None,
            capabilities: Capabilities {
                capability: vec![
                    "urn:ietf:params:netconf:base:1.0".to_string(),
                    "urn:ietf:params:netconf:base:1.1".to_string(),
                ],
            },
        };

        assert_eq!(hello.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_close_session() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <close-session/>
</rpc>
"#;

        let close_session = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::CloseSession,
        };
        assert_eq!(close_session.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_kill_session() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <kill-session>
    <session-id>69</session-id>
  </kill-session>
</rpc>
"#;
        let close_session = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::KillSession { session_id: 69 },
        };
        assert_eq!(close_session.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_get_config() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <get-config>
    <source>
      <running/>
    </source>
    <with-defaults xmlns="urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults">report-all</with-defaults>
  </get-config>
</rpc>
"#;
        let get_config = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_get_config(
                Datastore::Running,
                None,
                Some(WithDefaultsValue::ReportAll),
            ),
        };
        assert_eq!(get_config.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_get() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <get>
    <filter type="subtree"><top xmlns="https://example.com/schema/1.2/config"><users><user><name>fred</name></user></users></top></filter>
  </get>
</rpc>
"#;
        let filter = r#"<top xmlns="https://example.com/schema/1.2/config"><users><user><name>fred</name></user></users></top>"#;
        let get = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_get(Some(Filter::subtree(filter)), None),
        };
        assert_eq!(get.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_commit() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit/>
</rpc>
"#;
        let commit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_commit(None, None, None, None),
        };
        assert_eq!(commit.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit>
    <confirmed/>
    <confirm-timeout>120</confirm-timeout>
    <persist>persis,qqSADD</persist>
  </commit>
</rpc>
"#;
        let commit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_commit(
                Some(()),
                Some(120),
                Some("persis,qqSADD".to_string()),
                None,
            ),
        };
        assert_eq!(commit.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit>
    <persist-id>myid</persist-id>
  </commit>
</rpc>
"#;
        let commit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_commit(None, None, None, Some("myid".to_string())),
        };
        assert_eq!(commit.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_validate() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <validate>
    <source>
      <candidate/>
    </source>
  </validate>
</rpc>
"#;
        let validate = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::Validate {
                source: Source {
                    datastore: Datastore::Candidate,
                },
            },
        };
        assert_eq!(validate.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_create_subscription() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <create-subscription xmlns="urn:ietf:params:xml:ns:netconf:notification:1.0">
    <stream>NETCONF</stream>
    <startTime>|start|</startTime>
    <stopTime>|stop|</stopTime>
  </create-subscription>
</rpc>
"#;
        let start_time = OffsetDateTime::now_utc();
        let stop_time = start_time
            .checked_add(Duration::checked_seconds_f32(60.0).unwrap())
            .unwrap();
        let validate = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::CreateSubscription(CreateSubscription {
                xmlns: "urn:ietf:params:xml:ns:netconf:notification:1.0".to_string(),
                stream: Some("NETCONF".to_string()),
                filter: None,
                start_time: Some(start_time),
                stop_time: Some(stop_time),
            }),
        };
        let expected = expected
            .trim()
            .replace("|start|", start_time.format(&Rfc3339).unwrap().as_str())
            .replace("|stop|", stop_time.format(&Rfc3339).unwrap().as_str());
        assert_eq!(validate.to_string(), expected);
    }

    #[test]
    fn test_serialize_edit_config() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <edit-config>
    <target>
      <candidate/>
    </target>
    <default-operation>replace</default-operation>
    <test-option>test-only</test-option>
    <error-option>continue-on-error</error-option>
    <config><top xmlns="http://example.com/schema/1.2/config"><interface><name>Ethernet0/0</name><mtu>1500</mtu></interface></top></config>
  </edit-config>
</rpc>
"#;
        let config = r#"<top xmlns="http://example.com/schema/1.2/config"><interface><name>Ethernet0/0</name><mtu>1500</mtu></interface></top>"#;
        let edit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_edit_config(
                Datastore::Candidate,
                EditContent::Config(config.to_string()),
                Some(DefaultOperation::Replace),
                Some(TestOption::TestOnly),
                Some(ErrorOption::ContinueOnError),
            ),
        };
        assert_eq!(edit.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_edit_config_url_and_wrapped_config() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <edit-config>
    <target>
      <running/>
    </target>
    <url>ftp://myserver.example.com/router.cfg</url>
  </edit-config>
</rpc>
"#;
        let edit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_edit_config(
                Datastore::Running,
                EditContent::Url("ftp://myserver.example.com/router.cfg".to_string()),
                None,
                None,
                None,
            ),
        };
        assert_eq!(edit.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <edit-config>
    <target>
      <candidate/>
    </target>
    <config><system><host-name>darkstar</host-name></system></config>
  </edit-config>
</rpc>
"#;
        let edit = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_edit_config(
                Datastore::Candidate,
                EditContent::Config(
                    "<config>\n  <system><host-name>darkstar</host-name></system>\n</config>"
                        .to_string(),
                ),
                None,
                None,
                None,
            ),
        };
        assert_eq!(edit.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_copy_config() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <copy-config>
    <target>
      <startup/>
    </target>
    <source>
      <running/>
    </source>
  </copy-config>
</rpc>
"#;
        let copy = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_copy_config(Datastore::Running.into(), Datastore::Startup),
        };
        assert_eq!(copy.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_copy_config_url_and_inline() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <copy-config>
    <target>
      <url>ftp://myserver.example.com/router.cfg</url>
    </target>
    <source>
      <running/>
    </source>
  </copy-config>
</rpc>
"#;
        let copy = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_copy_config(
                Datastore::Running.into(),
                Datastore::Url("ftp://myserver.example.com/router.cfg".to_string()),
            ),
        };
        assert_eq!(copy.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <copy-config>
    <target>
      <candidate/>
    </target>
    <source>
      <config><top xmlns="http://example.com/schema/1.2/config"/></config>
    </source>
  </copy-config>
</rpc>
"#;
        let copy = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_copy_config(
                CopySource::Config(
                    r#"<top xmlns="http://example.com/schema/1.2/config"/>"#.to_string(),
                ),
                Datastore::Candidate,
            ),
        };
        assert_eq!(copy.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_delete_config() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <delete-config>
    <target>
      <startup/>
    </target>
  </delete-config>
</rpc>
"#;
        let delete = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_delete_config(Datastore::Startup),
        };
        assert_eq!(delete.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_lock_unlock() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <lock>
    <target>
      <candidate/>
    </target>
  </lock>
</rpc>
"#;
        let lock = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_lock(Datastore::Candidate),
        };
        assert_eq!(lock.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <unlock>
    <target>
      <candidate/>
    </target>
  </unlock>
</rpc>
"#;
        let unlock = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_unlock(Datastore::Candidate),
        };
        assert_eq!(unlock.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_discard_changes() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <discard-changes/>
</rpc>
"#;
        let discard = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_discard_changes(),
        };
        assert_eq!(discard.to_string(), expected.trim());
    }

    #[test]
    fn test_serialize_cancel_commit() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <cancel-commit/>
</rpc>
"#;
        let cancel = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_cancel_commit(None),
        };
        assert_eq!(cancel.to_string(), expected.trim());

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <cancel-commit>
    <persist-id>myid</persist-id>
  </cancel-commit>
</rpc>
"#;
        let cancel = Rpc {
            xmlns: "urn:ietf:params:xml:ns:netconf:base:1.0".to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation: RpcOperation::new_cancel_commit(Some("myid".to_string())),
        };
        assert_eq!(cancel.to_string(), expected.trim());
    }
}
