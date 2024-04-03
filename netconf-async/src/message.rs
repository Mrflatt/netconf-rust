#![allow(dead_code)]
use crate::{error, NETCONF_URN};
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
            RpcOperation::GetConfig { .. } | RpcOperation::Get { .. } => {
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

#[derive(Debug, Serialize)]
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
    App,
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
    use time::format_description::well_known::Rfc3339;
    use time::Duration;

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
    <error-type>app</error-type>
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
    <with-defaults xmlns="urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults">
      report-all
    </with-defaults>
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
    <filter type="subtree">
      <top xmlns="https://example.com/schema/1.2/config"><users><user><name>fred</name></user></users></top>
    </filter>
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
}
