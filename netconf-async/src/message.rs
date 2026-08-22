//! NETCONF messages: `<hello>`, `<rpc>`, `<rpc-reply>`, and operation bodies.

use crate::{NETCONF_URN, error};
use core::fmt;
use core::fmt::Display;
use core::ops::Add;
use core::str::FromStr;
use core::time::Duration;
use quick_xml::se::Serializer;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Deserialize `xml`, retrying without namespace prefixes if the first pass fails.
///
/// Devices that answer with `<nc:hello>` / `<nc:rpc-reply>` are common enough
/// that a strict, prefix-sensitive parse would refuse to talk to them. The
/// retry only runs on failure, so conforming devices pay nothing.
pub(crate) fn from_xml<T: DeserializeOwned>(xml: &str) -> error::NetconfClientResult<T> {
    match quick_xml::de::from_str(xml) {
        Ok(parsed) => Ok(parsed),
        Err(err) => match strip_namespace_prefixes(xml) {
            Some(stripped) => quick_xml::de::from_str(&stripped).map_err(|_| err.into()),
            None => Err(err.into()),
        },
    }
}

/// Rewrite every element name to its local part, dropping the namespace prefix.
///
/// Returns `None` when the document cannot be re-serialized, in which case the
/// caller keeps the original parse error.
fn strip_namespace_prefixes(xml: &str) -> Option<String> {
    use quick_xml::events::{BytesEnd, BytesStart, Event};
    use quick_xml::{Reader, Writer};

    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::new());
    let mut changed = false;
    loop {
        match reader.read_event().ok()? {
            Event::Eof => break,
            Event::Start(event) => {
                let (name, renamed) = local_name(&event);
                changed |= renamed;
                let mut start = BytesStart::new(name);
                start.extend_attributes(event.attributes().filter_map(Result::ok));
                writer.write_event(Event::Start(start)).ok()?;
            }
            Event::Empty(event) => {
                let (name, renamed) = local_name(&event);
                changed |= renamed;
                let mut start = BytesStart::new(name);
                start.extend_attributes(event.attributes().filter_map(Result::ok));
                writer.write_event(Event::Empty(start)).ok()?;
            }
            Event::End(event) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
                changed |= name.len() != event.name().as_ref().len();
                writer.write_event(Event::End(BytesEnd::new(name))).ok()?;
            }
            event => writer.write_event(event).ok()?,
        }
    }
    if !changed {
        return None;
    }
    String::from_utf8(writer.into_inner()).ok()
}

fn local_name(event: &quick_xml::events::BytesStart<'_>) -> (String, bool) {
    let local = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
    let renamed = local.len() != event.name().as_ref().len();
    (local, renamed)
}

/// Capability URN without its `?query` parameters.
pub(crate) fn capability_id(capability: &str) -> &str {
    capability
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(capability)
}

/// Caller-supplied XML that must reach the device verbatim.
///
/// The serializer escapes every string it writes, which would corrupt a config
/// or filter payload. Serializing a placeholder and substituting the original
/// afterwards keeps the payload intact without disturbing the escaping of
/// neighbouring elements such as `<url>`.
#[derive(Debug)]
struct RawXml {
    xml: String,
    placeholder: String,
}

impl RawXml {
    fn new(xml: &str) -> Self {
        RawXml {
            xml: xml.trim().to_string(),
            placeholder: format!("netconf-raw-{}", Uuid::new_v4().simple()),
        }
    }
}

impl Serialize for RawXml {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The placeholder is `[a-z0-9-]` only, so the serializer writes it as-is.
        serializer.serialize_str(&self.placeholder)
    }
}

/// `<hello>` advertisement ([RFC6241 8.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.1)).
///
/// [`Hello::new`] advertises `:base:1.0` and `:base:1.1`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename(serialize = "hello"))]
pub struct Hello {
    #[serde(rename = "@xmlns", default, skip_serializing_if = "Option::is_none")]
    xmlns: Option<String>,
    capabilities: Capabilities,
    #[serde(
        rename = "session-id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    session_id: Option<u64>,
}

impl Hello {
    /// Client hello advertising `:base:1.0` and `:base:1.1`.
    pub fn new() -> Hello {
        Hello {
            xmlns: Some(NETCONF_URN.to_string()),
            session_id: None,
            capabilities: Capabilities {
                capability: vec![
                    crate::NETCONF_BASE_10_CAP.to_string(),
                    crate::NETCONF_BASE_11_CAP.to_string(),
                ],
            },
        }
    }

    /// Capability URNs advertised in this hello.
    pub fn capabilities(&self) -> Vec<String> {
        self.capabilities.capability.clone()
    }

    /// True if `capability` was advertised, ignoring any `?query` parameters.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities
            .capability
            .iter()
            .any(|cap| capability_id(cap) == capability)
    }

    /// Server-assigned session-id, present only on the server hello.
    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }
}

impl Display for Hello {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = String::with_capacity(206);
        self.serialize(Serializer::new(&mut buffer))
            .map_err(|_| fmt::Error)?;
        f.write_str(&buffer)
    }
}

/// `<capabilities>` list inside `<hello>`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Capabilities {
    capability: Vec<String>,
}

/// `<rpc>` envelope with a generated `message-id` ([RFC6241 4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.1)).
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
    /// Wrap `operation` and assign a UUID `message-id`.
    pub fn new_with_operation(operation: RpcOperation) -> Rpc {
        Rpc {
            xmlns: NETCONF_URN.to_string(),
            message_id: Uuid::new_v4().to_string(),
            operation,
        }
    }

    /// `message-id` carried by this request.
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
}

impl Display for Rpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = String::with_capacity(256);
        let mut ser = Serializer::with_root(&mut buffer, Some("rpc")).map_err(|_| fmt::Error)?;
        ser.indent(' ', 2);
        self.serialize(ser).map_err(|_| fmt::Error)?;
        for raw in self.operation.raw_payloads() {
            buffer = buffer.replace(&raw.placeholder, &raw.xml);
        }
        f.write_str(&buffer)
    }
}

/// Body of an `<rpc>` ([RFC6241 7](https://www.rfc-editor.org/rfc/rfc6241.html#section-7)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum RpcOperation {
    /// `<close-session>`.
    CloseSession,
    /// `<kill-session>`.
    KillSession {
        /// Session to terminate.
        #[serde(rename = "session-id")]
        session_id: u64,
    },
    /// `<validate>`.
    Validate {
        /// Datastore to validate.
        source: Source,
    },
    /// `<get-config>`.
    GetConfig(GetConfig),
    /// `<get>`.
    Get(Get),
    /// `<edit-config>`.
    EditConfig(EditConfig),
    /// `<copy-config>`.
    CopyConfig(CopyConfig),
    /// `<delete-config>`.
    DeleteConfig {
        /// Datastore to delete.
        target: Target,
    },
    /// `<lock>`.
    Lock {
        /// Datastore to lock.
        target: Target,
    },
    /// `<unlock>`.
    Unlock {
        /// Datastore to unlock.
        target: Target,
    },
    /// `<discard-changes>`.
    DiscardChanges,
    /// `<cancel-commit>`.
    CancelCommit(CancelCommit),
    /// `<commit>`.
    Commit(Commit),
    /// `<create-subscription>`.
    CreateSubscription(CreateSubscription),
}

impl RpcOperation {
    /// `<get-config>` ([RFC6241 7.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.1)).
    pub fn new_get_config(
        datastore: Datastore,
        filter: Option<Filter>,
        defaults: Option<WithDefaultsValue>,
    ) -> RpcOperation {
        RpcOperation::GetConfig(GetConfig {
            source: Source { datastore },
            filter,
            with_defaults: defaults.map(WithDefaults::new),
        })
    }

    /// `<get>` ([RFC6241 7.7](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.7)).
    pub fn new_get(filter: Option<Filter>, defaults: Option<WithDefaultsValue>) -> RpcOperation {
        RpcOperation::Get(Get {
            filter,
            with_defaults: defaults.map(WithDefaults::new),
        })
    }

    /// `<commit>`, optionally confirmed ([RFC6241 8.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3), [8.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4)).
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

    /// `<create-subscription>` ([RFC5277 2.1.1](https://www.rfc-editor.org/rfc/rfc5277.html#section-2.1.1)).
    pub fn new_create_subscription(
        stream: Option<&str>,
        filter: Option<Filter>,
        duration: Option<Duration>,
    ) -> RpcOperation {
        let (start_time, stop_time) = if let Some(duration) = duration {
            let now = OffsetDateTime::now_utc();
            (Some(now), Some(now.add(duration)))
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

    /// `<edit-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
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

    /// `<copy-config>` ([RFC6241 7.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.3)).
    pub fn new_copy_config(source: CopySource, target: Datastore) -> RpcOperation {
        RpcOperation::CopyConfig(CopyConfig {
            target: Target { datastore: target },
            source: CopySourceXml {
                value: CopySourceKind::from(source),
            },
        })
    }

    /// `<delete-config>` ([RFC6241 7.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.4)).
    pub fn new_delete_config(target: Datastore) -> RpcOperation {
        RpcOperation::DeleteConfig {
            target: Target { datastore: target },
        }
    }

    /// `<lock>` ([RFC6241 7.5](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.5)).
    pub fn new_lock(target: Datastore) -> RpcOperation {
        RpcOperation::Lock {
            target: Target { datastore: target },
        }
    }

    /// `<unlock>` ([RFC6241 7.6](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.6)).
    pub fn new_unlock(target: Datastore) -> RpcOperation {
        RpcOperation::Unlock {
            target: Target { datastore: target },
        }
    }

    /// `<discard-changes>` ([RFC6241 8.3.4.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3.4.2)).
    pub fn new_discard_changes() -> RpcOperation {
        RpcOperation::DiscardChanges
    }

    /// `<cancel-commit>` ([RFC6241 8.4.4.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4.4.1)).
    pub fn new_cancel_commit(persist_id: Option<String>) -> RpcOperation {
        RpcOperation::CancelCommit(CancelCommit { persist_id })
    }

    /// Verbatim payloads to splice back in after serialization.
    fn raw_payloads(&self) -> Vec<&RawXml> {
        match self {
            RpcOperation::Get(get) => get.filter.iter().map(|f| &f.filter).collect(),
            RpcOperation::GetConfig(get) => get.filter.iter().map(|f| &f.filter).collect(),
            RpcOperation::EditConfig(edit) => edit.config.iter().map(|c| &c.xml).collect(),
            RpcOperation::CopyConfig(copy) => match &copy.source.value {
                CopySourceKind::Config(raw) => vec![raw],
                _ => Vec::new(),
            },
            RpcOperation::CreateSubscription(sub) => sub.filter.iter().map(|f| &f.filter).collect(),
            _ => Vec::new(),
        }
    }
}

/// `<commit>` body ([RFC6241 8.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.3), [8.4](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.4)).
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

/// `<get>` body ([RFC6241 7.7](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.7)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Get {
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    with_defaults: Option<WithDefaults>,
}

/// `<get-config>` body ([RFC6241 7.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.1)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct GetConfig {
    source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    with_defaults: Option<WithDefaults>,
}

/// `with-defaults` element ([RFC6243](https://www.rfc-editor.org/rfc/rfc6243.html)).
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WithDefaults {
    #[serde(rename = "@xmlns")]
    xmlns: String,
    #[serde(rename = "$text")]
    value: WithDefaultsValue,
}

impl WithDefaults {
    fn new(value: WithDefaultsValue) -> Self {
        WithDefaults {
            xmlns: "urn:ietf:params:xml:ns:yang:ietf-netconf-with-defaults".to_string(),
            value,
        }
    }
}

/// `with-defaults` mode ([RFC6243](https://www.rfc-editor.org/rfc/rfc6243.html)).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WithDefaultsValue {
    /// Report every default.
    ReportAll,
    /// Report every default, tagged.
    ReportAllTagged,
    /// Omit values equal to their default.
    Trim,
    /// Report only explicitly set values.
    Explicit,
}

impl FromStr for WithDefaultsValue {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "report-all" => Ok(WithDefaultsValue::ReportAll),
            "report-all-tagged" => Ok(WithDefaultsValue::ReportAllTagged),
            "trim" => Ok(WithDefaultsValue::Trim),
            "explicit" => Ok(WithDefaultsValue::Explicit),
            _ => Err(error::NetconfClientError::new(format!(
                "unknown with-defaults value: {s}"
            ))),
        }
    }
}

/// Source datastore of `<get-config>` / `<validate>` / `<copy-config>`.
#[derive(Debug, Serialize)]
pub struct Source {
    /// Datastore being read.
    #[serde(rename = "$value")]
    pub datastore: Datastore,
}

/// Destination datastore of `<edit-config>` / `<copy-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Serialize)]
pub struct Target {
    /// Datastore being written.
    #[serde(rename = "$value")]
    pub datastore: Datastore,
}

/// Configuration datastore ([RFC6241 5.1](https://www.rfc-editor.org/rfc/rfc6241.html#section-5.1)).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Datastore {
    /// `<candidate/>`.
    Candidate,
    /// `<running/>`.
    Running,
    /// `<startup/>`.
    Startup,
    /// `<url>`, requires the `:url` capability.
    Url(String),
}

/// URL schemes accepted for the `:url` capability ([RFC6241 8.8](https://www.rfc-editor.org/rfc/rfc6241.html#section-8.8)).
const URL_SCHEMES: [&str; 6] = ["http", "https", "ftp", "ftps", "file", "sftp"];

impl FromStr for Datastore {
    type Err = error::NetconfClientError;

    /// Parse a datastore name. URLs keep their original case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        match trimmed.to_lowercase().as_str() {
            "running" => Ok(Datastore::Running),
            "candidate" => Ok(Datastore::Candidate),
            "startup" => Ok(Datastore::Startup),
            _ => {
                let scheme = trimmed
                    .split_once("://")
                    .filter(|(_, rest)| !rest.is_empty())
                    .map(|(scheme, _)| scheme.to_lowercase());
                match scheme {
                    Some(scheme) if URL_SCHEMES.contains(&scheme.as_str()) => {
                        Ok(Datastore::Url(trimmed.to_string()))
                    }
                    _ => Err(error::NetconfClientError::UnknownDatastore {
                        expected: vec![
                            "running".to_string(),
                            "candidate".to_string(),
                            "startup".to_string(),
                            URL_SCHEMES.join("|"),
                        ],
                        unknown: trimmed.to_string(),
                    }),
                }
            }
        }
    }
}

/// Payload of `<edit-config>`: inline config XML or a URL ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug)]
pub enum EditContent {
    /// Config XML, with or without a `<config>` wrapper.
    Config(String),
    /// URL to fetch the config from.
    Url(String),
}

/// `default-operation` of `<edit-config>` ([RFC6241 7.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-7.2)).
#[derive(Debug, Clone, Copy)]
pub enum DefaultOperation {
    /// Merge into the target.
    Merge,
    /// Replace the target.
    Replace,
    /// Only apply explicit `operation` attributes.
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

impl Serialize for DefaultOperation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for DefaultOperation {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
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
    /// Validate, then apply.
    TestThenSet,
    /// Apply without validating.
    Set,
    /// Validate only.
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

impl Serialize for TestOption {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for TestOption {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
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
    /// Stop at the first error.
    StopOnError,
    /// Keep going past errors.
    ContinueOnError,
    /// Roll the whole edit back on error.
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

impl Serialize for ErrorOption {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for ErrorOption {
    type Err = error::NetconfClientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
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
    xml: RawXml,
}

impl InlineConfig {
    fn new(xml: &str) -> Self {
        Self {
            xml: RawXml::new(strip_config_wrapper(xml)),
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
#[derive(Debug)]
pub enum CopySource {
    /// `<candidate/>`.
    Candidate,
    /// `<running/>`.
    Running,
    /// `<startup/>`.
    Startup,
    /// `<url>`.
    Url(String),
    /// Inline `<config>`, with or without the wrapper.
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
#[serde(rename_all = "lowercase")]
enum CopySourceKind {
    Candidate,
    Running,
    Startup,
    Url(String),
    Config(RawXml),
}

impl From<CopySource> for CopySourceKind {
    fn from(source: CopySource) -> Self {
        match source {
            CopySource::Candidate => Self::Candidate,
            CopySource::Running => Self::Running,
            CopySource::Startup => Self::Startup,
            CopySource::Url(url) => Self::Url(url),
            CopySource::Config(xml) => Self::Config(RawXml::new(strip_config_wrapper(&xml))),
        }
    }
}

#[derive(Debug, Serialize)]
struct CopySourceXml {
    #[serde(rename = "$value")]
    value: CopySourceKind,
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

/// Subtree filter for `<get>` / `<get-config>` ([RFC6241 6](https://www.rfc-editor.org/rfc/rfc6241.html#section-6)).
#[derive(Debug, Serialize)]
pub struct Filter {
    #[serde(rename = "@type")]
    filter_type: String,
    #[serde(rename = "$value")]
    filter: RawXml,
}

impl Filter {
    /// Build a subtree filter from XML, which is sent to the device verbatim.
    pub fn subtree(filter: &str) -> Filter {
        Filter {
            filter_type: "subtree".to_string(),
            filter: RawXml::new(filter),
        }
    }
}

/// `<rpc-reply>` ([RFC6241 4.2](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.2)).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", rename(serialize = "rpc-reply"))]
pub struct RpcReply {
    #[serde(
        rename = "@message-id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rpc_error: Option<Vec<Error>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<()>,
}

impl RpcReply {
    /// True when the reply has `<ok>` and no `<rpc-error>`.
    pub fn is_ok(&self) -> bool {
        self.ok.is_some() && self.rpc_error.is_none()
    }

    /// True when the reply contains at least one `<rpc-error>`.
    pub fn has_errors(&self) -> bool {
        self.rpc_error.is_some()
    }

    /// Every `<rpc-error>` in the reply, empty when the RPC succeeded.
    pub fn errors(&self) -> &[Error] {
        self.rpc_error.as_deref().unwrap_or(&[])
    }

    /// `message-id` echoed from the request, if the device sent one.
    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }
}

impl Display for RpcReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = String::with_capacity(512);
        let mut ser = Serializer::new(&mut buffer);
        ser.indent(' ', 2);
        self.serialize(ser).map_err(|_| fmt::Error)?;
        f.write_str(&buffer)
    }
}

impl std::error::Error for RpcReply {}

/// One `<rpc-error>` from a reply ([RFC6241 4.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.3)).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename = "rpc-error", rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Error {
    /// `<error-severity>`.
    pub error_severity: ErrorSeverity,
    /// `<error-type>`, the protocol layer that failed.
    pub error_type: ErrorType,
    /// `<error-tag>`, the machine-readable reason.
    pub error_tag: ErrorTag,
    /// `<error-app-tag>`, a data-model-specific reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_app_tag: Option<String>,
    /// `<error-path>`, the offending node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_path: Option<String>,
    /// `<error-message>`, human-readable text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// `<error-info>`, protocol- or data-model-specific detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_info: Option<ErrorInfo>,
}

/// Builds an enum that round-trips through XML text and keeps unknown values.
///
/// Vendors ship tags outside RFC 6241, and losing the whole reply to an
/// unrecognised `<error-tag>` would hide the error the device is reporting.
macro_rules! open_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A value this crate does not know.
            Other(String),
        }

        impl $name {
            /// The value as it appears on the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value.trim() {
                    $($text => Self::$variant,)*
                    other => Self::Other(other.to_string()),
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ok(Self::from(String::deserialize(deserializer)?.as_str()))
            }
        }
    };
}

open_enum!(
    /// `<error-type>` ([RFC6241 4.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.3)).
    ErrorType {
        /// Secure transport layer.
        Transport => "transport",
        /// RPC layer.
        Rpc => "rpc",
        /// Protocol operation layer.
        Protocol => "protocol",
        /// Content layer.
        Application => "application",
    }
);

open_enum!(
    /// `<error-severity>` ([RFC6241 4.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.3)).
    ErrorSeverity {
        /// The operation failed.
        Error => "error",
        /// Advisory only.
        Warning => "warning",
    }
);

open_enum!(
    /// `<error-tag>` ([RFC6241 appendix A](https://www.rfc-editor.org/rfc/rfc6241.html#appendix-A)).
    ErrorTag {
        /// Resource is in use.
        InUse => "in-use",
        /// Value is out of range or otherwise invalid.
        InvalidValue => "invalid-value",
        /// Request or reply is too large.
        TooBig => "too-big",
        /// A required attribute is missing.
        MissingAttribute => "missing-attribute",
        /// An attribute has an unexpected value.
        BadAttribute => "bad-attribute",
        /// An attribute is not recognised.
        UnknownAttribute => "unknown-attribute",
        /// A required element is missing.
        MissingElement => "missing-element",
        /// An element has an unexpected value.
        BadElement => "bad-element",
        /// An element is not recognised.
        UnknownElement => "unknown-element",
        /// A namespace is not recognised.
        UnknownNamespace => "unknown-namespace",
        /// Access to the resource was denied.
        AccessDenied => "access-denied",
        /// The lock is already held.
        LockDenied => "lock-denied",
        /// The device is out of resources.
        ResourceDenied => "resource-denied",
        /// Rollback was requested but failed.
        RollbackFailed => "rollback-failed",
        /// The data already exists.
        DataExists => "data-exists",
        /// The data does not exist.
        DataMissing => "data-missing",
        /// The operation is not supported.
        OperationNotSupported => "operation-not-supported",
        /// The operation failed for an unspecified reason.
        OperationFailed => "operation-failed",
        /// The operation was only partly applied.
        PartialOperation => "partial-operation",
        /// The message is not well-formed.
        MalformedMessage => "malformed-message",
    }
);

/// `<error-info>` detail ([RFC6241 4.3](https://www.rfc-editor.org/rfc/rfc6241.html#section-4.3)).
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ErrorInfo {
    /// Element that caused the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_element: Option<String>,
    /// Attribute that caused the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_attribute: Option<String>,
    /// Namespace that caused the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_namespace: Option<String>,
    /// Element that was applied successfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok_element: Option<String>,
    /// Element that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err_element: Option<String>,
    /// Element that was not attempted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noop_element: Option<String>,
    /// Session holding the lock, for `lock-denied`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,
}

/// `<create-subscription>` body ([RFC5277 2.1.1](https://www.rfc-editor.org/rfc/rfc5277.html#section-2.1.1)).
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

    fn rpc(operation: RpcOperation) -> Rpc {
        Rpc {
            xmlns: NETCONF_URN.to_string(),
            message_id: "c1be0e7f-3cbc-413f-8aa8-18ed663221d4".to_string(),
            operation,
        }
    }

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
        assert_eq!(reply.errors().len(), 2);
        assert_eq!(reply.errors()[0].error_type, ErrorType::Protocol);
        assert_eq!(reply.errors()[1].error_type, ErrorType::Application);

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
        assert_eq!(reply.errors()[0].error_tag, ErrorTag::UnknownElement);
        assert_eq!(
            reply.errors()[0].error_info.as_ref().unwrap().bad_element,
            Some("startup".to_string())
        );

        let reply = r#"
<rpc-reply message-id="c60e637d-0f79-41ea-ad09-a5ee02f08434">
  <data>
    <configure xmlns="urn:nokia.com:sros:ns:yang:sr:conf" xmlns:nokia-attr="urn:nokia.com:sros:ns:yang:sr:attributes">
      <port>
        <port-id>1/1/2</port-id>
      </port>
      <system>
        <time>
          <ntp>
            <admin-state>enable</admin-state>
          </ntp>
        </time>
      </system>
    </configure>
  </data>
</rpc-reply>
        "#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(!reply.has_errors());
        assert!(!reply.is_ok());

        let reply = r#"
<?xml version="1.0" encoding="UTF-8"?>
<rpc-reply message-id="938f1c28-e6e3-4641-a4d0-383d9ef1a280" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <ok/>
</rpc-reply>
"#;
        let reply: RpcReply = from_str(reply).unwrap();
        assert!(reply.is_ok());
    }

    #[test]
    fn reply_without_message_id_parses() {
        let reply =
            r#"<rpc-reply xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><ok/></rpc-reply>"#;
        let reply: RpcReply = from_xml(reply).unwrap();
        assert!(reply.is_ok());
        assert_eq!(reply.message_id(), None);
    }

    #[test]
    fn namespace_prefixed_reply_parses() {
        let reply = r#"<nc:rpc-reply xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0" message-id="4">
  <nc:rpc-error>
    <nc:error-type>protocol</nc:error-type>
    <nc:error-tag>access-denied</nc:error-tag>
    <nc:error-severity>error</nc:error-severity>
  </nc:rpc-error>
</nc:rpc-reply>"#;
        let reply: RpcReply = from_xml(reply).unwrap();
        assert!(reply.has_errors());
        assert_eq!(reply.errors()[0].error_tag, ErrorTag::AccessDenied);
    }

    #[test]
    fn vendor_error_tag_is_preserved() {
        let reply = r#"<rpc-reply message-id="1">
  <rpc-error>
    <error-type>application</error-type>
    <error-tag>vendor-specific-failure</error-tag>
    <error-severity>error</error-severity>
  </rpc-error>
</rpc-reply>"#;
        let reply: RpcReply = from_xml(reply).unwrap();
        assert_eq!(
            reply.errors()[0].error_tag,
            ErrorTag::Other("vendor-specific-failure".to_string())
        );
        assert_eq!(
            reply.errors()[0].error_tag.as_str(),
            "vendor-specific-failure"
        );
    }

    #[test]
    fn namespace_prefixed_hello_parses() {
        let hello = r#"<nc:hello xmlns:nc="urn:ietf:params:xml:ns:netconf:base:1.0">
  <nc:capabilities>
    <nc:capability>urn:ietf:params:netconf:base:1.0</nc:capability>
    <nc:capability>urn:ietf:params:netconf:base:1.1</nc:capability>
  </nc:capabilities>
  <nc:session-id>42</nc:session-id>
</nc:hello>"#;
        let hello: Hello = from_xml(hello).unwrap();
        assert_eq!(hello.session_id(), Some(42));
        assert!(hello.has_capability(crate::NETCONF_BASE_11_CAP));
    }

    #[test]
    fn hello_without_xmlns_attribute_parses() {
        let hello = r#"<hello><capabilities><capability>urn:ietf:params:netconf:base:1.1</capability></capabilities></hello>"#;
        let hello: Hello = from_xml(hello).unwrap();
        assert!(hello.has_capability(crate::NETCONF_BASE_11_CAP));
        assert_eq!(hello.session_id(), None);
    }

    #[test]
    fn hello_capability_ignores_query_parameters() {
        let hello = r#"<hello xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><capabilities>
  <capability>urn:ietf:params:netconf:capability:url:1.0?scheme=http,ftp,file</capability>
</capabilities></hello>"#;
        let hello: Hello = from_xml(hello).unwrap();
        assert!(hello.has_capability(crate::URL_CAP));
    }

    #[test]
    fn datastore_from_str_keeps_url_case() {
        assert!(matches!(
            Datastore::from_str("RUNNING").unwrap(),
            Datastore::Running
        ));
        let Datastore::Url(url) =
            Datastore::from_str("ftp://Server.EXAMPLE.com/Router.CFG").unwrap()
        else {
            panic!("expected a url datastore");
        };
        assert_eq!(url, "ftp://Server.EXAMPLE.com/Router.CFG");
        assert!(Datastore::from_str("httpfoo").is_err());
        assert!(Datastore::from_str("http://").is_err());
    }

    #[test]
    fn url_query_parameters_stay_escaped() {
        let xml = rpc(RpcOperation::new_edit_config(
            Datastore::Running,
            EditContent::Url("ftp://host/router.cfg?a=1&b=2".to_string()),
            None,
            None,
            None,
        ))
        .to_string();
        assert!(
            xml.contains("<url>ftp://host/router.cfg?a=1&amp;b=2</url>"),
            "ampersand must stay escaped:\n{xml}"
        );

        let xml = rpc(RpcOperation::new_copy_config(
            CopySource::Url("ftp://host/router.cfg?a=1&b=2".to_string()),
            Datastore::Running,
        ))
        .to_string();
        assert!(
            xml.contains("<url>ftp://host/router.cfg?a=1&amp;b=2</url>"),
            "ampersand must stay escaped:\n{xml}"
        );
    }

    #[test]
    fn config_payload_reaches_the_device_verbatim() {
        let config = r#"<top xmlns="http://example.com/1.2"><name>a &amp; b</name><cdata><![CDATA[<x/>]]></cdata></top>"#;
        let xml = rpc(RpcOperation::new_edit_config(
            Datastore::Candidate,
            EditContent::Config(config.to_string()),
            None,
            None,
            None,
        ))
        .to_string();
        assert!(xml.contains(config), "payload was rewritten:\n{xml}");
    }

    #[test]
    fn filter_payload_reaches_the_device_verbatim() {
        let filter = r#"<top><name>a &amp; b</name></top>"#;
        let xml = rpc(RpcOperation::new_get(Some(Filter::subtree(filter)), None)).to_string();
        assert!(xml.contains(filter), "filter was rewritten:\n{xml}");
    }

    #[test]
    fn filter_with_trailing_backslash_is_not_a_panic() {
        let filter = Filter::subtree(r"<top>C:\temp\</top>");
        let xml = rpc(RpcOperation::new_get(Some(filter), None)).to_string();
        assert!(xml.contains(r"<top>C:\temp\</top>"), "{xml}");
    }

    #[test]
    fn test_serialize_hello() {
        let expected = r#"<hello xmlns="urn:ietf:params:xml:ns:netconf:base:1.0"><capabilities><capability>urn:ietf:params:netconf:base:1.0</capability><capability>urn:ietf:params:netconf:base:1.1</capability></capabilities></hello>"#;
        assert_eq!(Hello::new().to_string(), expected);
    }

    #[test]
    fn test_serialize_close_session() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <close-session/>
</rpc>
"#;
        assert_eq!(rpc(RpcOperation::CloseSession).to_string(), expected.trim());
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
        assert_eq!(
            rpc(RpcOperation::KillSession { session_id: 69 }).to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_get_config(
                Datastore::Running,
                None,
                Some(WithDefaultsValue::ReportAll),
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_get(Some(Filter::subtree(filter)), None)).to_string(),
            expected.trim()
        );
    }

    #[test]
    fn test_serialize_commit() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit/>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_commit(None, None, None, None)).to_string(),
            expected.trim()
        );

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit>
    <confirmed/>
    <confirm-timeout>120</confirm-timeout>
    <persist>persis,qqSADD</persist>
  </commit>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_commit(
                Some(()),
                Some(120),
                Some("persis,qqSADD".to_string()),
                None,
            ))
            .to_string(),
            expected.trim()
        );

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <commit>
    <persist-id>myid</persist-id>
  </commit>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_commit(
                None,
                None,
                None,
                Some("myid".to_string())
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::Validate {
                source: Source {
                    datastore: Datastore::Candidate,
                },
            })
            .to_string(),
            expected.trim()
        );
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
        let subscription = rpc(RpcOperation::CreateSubscription(CreateSubscription {
            xmlns: "urn:ietf:params:xml:ns:netconf:notification:1.0".to_string(),
            stream: Some("NETCONF".to_string()),
            filter: None,
            start_time: Some(start_time),
            stop_time: Some(stop_time),
        }));
        let expected = expected
            .trim()
            .replace("|start|", start_time.format(&Rfc3339).unwrap().as_str())
            .replace("|stop|", stop_time.format(&Rfc3339).unwrap().as_str());
        assert_eq!(subscription.to_string(), expected);
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
        assert_eq!(
            rpc(RpcOperation::new_edit_config(
                Datastore::Candidate,
                EditContent::Config(config.to_string()),
                Some(DefaultOperation::Replace),
                Some(TestOption::TestOnly),
                Some(ErrorOption::ContinueOnError),
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_edit_config(
                Datastore::Running,
                EditContent::Url("ftp://myserver.example.com/router.cfg".to_string()),
                None,
                None,
                None,
            ))
            .to_string(),
            expected.trim()
        );

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
        assert_eq!(
            rpc(RpcOperation::new_edit_config(
                Datastore::Candidate,
                EditContent::Config(
                    "<config>\n  <system><host-name>darkstar</host-name></system>\n</config>"
                        .to_string(),
                ),
                None,
                None,
                None,
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_copy_config(
                Datastore::Running.into(),
                Datastore::Startup
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_copy_config(
                Datastore::Running.into(),
                Datastore::Url("ftp://myserver.example.com/router.cfg".to_string()),
            ))
            .to_string(),
            expected.trim()
        );

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
        assert_eq!(
            rpc(RpcOperation::new_copy_config(
                CopySource::Config(
                    r#"<top xmlns="http://example.com/schema/1.2/config"/>"#.to_string(),
                ),
                Datastore::Candidate,
            ))
            .to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_delete_config(Datastore::Startup)).to_string(),
            expected.trim()
        );
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
        assert_eq!(
            rpc(RpcOperation::new_lock(Datastore::Candidate)).to_string(),
            expected.trim()
        );

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <unlock>
    <target>
      <candidate/>
    </target>
  </unlock>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_unlock(Datastore::Candidate)).to_string(),
            expected.trim()
        );
    }

    #[test]
    fn test_serialize_discard_changes() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <discard-changes/>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_discard_changes()).to_string(),
            expected.trim()
        );
    }

    #[test]
    fn test_serialize_cancel_commit() {
        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <cancel-commit/>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_cancel_commit(None)).to_string(),
            expected.trim()
        );

        let expected = r#"
<rpc message-id="c1be0e7f-3cbc-413f-8aa8-18ed663221d4" xmlns="urn:ietf:params:xml:ns:netconf:base:1.0">
  <cancel-commit>
    <persist-id>myid</persist-id>
  </cancel-commit>
</rpc>
"#;
        assert_eq!(
            rpc(RpcOperation::new_cancel_commit(Some("myid".to_string()))).to_string(),
            expected.trim()
        );
    }
}
