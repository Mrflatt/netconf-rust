use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Shared connection fields on every tool.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct ConnectionArgs {
    /// Device host. Optional `:port` (`192.0.2.1:830`, `[2001:db8::1]:830`).
    /// Default port is 830 or ssh_config `Port`.
    pub host: String,
    /// SSH username. Falls back to NETCONF_USERNAME / ssh_config.
    #[serde(default)]
    pub username: Option<String>,
    /// SSH password. Falls back to NETCONF_PASSWORD / IdentityFile / agent. Never defaulted.
    #[serde(default)]
    pub password: Option<String>,
    /// Per-RPC timeout in seconds.
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Optional path to write reply XML instead of returning it inline.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct OutputFileArg {
    /// Write reply XML to this path instead of returning it inline.
    /// Use for large get / get-config / get-schema / rpc replies.
    #[serde(default)]
    pub output_file: Option<String>,
}

impl ConnectionArgs {
    pub fn connect_params(&self) -> crate::ConnectParams {
        crate::ConnectParams {
            host: self.host.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            timeout: self.timeout,
            allowed_subnets: crate::AllowedNets::allow_all(),
        }
    }
}

/// `<get>` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Subtree filter XML. Do not include a `<filter>` wrapper.
    #[serde(default)]
    pub filter: Option<String>,
    /// with-defaults mode: report-all, report-all-tagged, trim, explicit.
    #[serde(default)]
    pub with_defaults: Option<String>,
    #[serde(flatten)]
    pub output: OutputFileArg,
}

/// `<get-config>` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetConfigArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Source datastore: running (default), candidate, startup.
    #[serde(default)]
    pub source: Option<String>,
    /// Subtree filter XML. Do not include a `<filter>` wrapper.
    #[serde(default)]
    pub filter: Option<String>,
    /// with-defaults mode: report-all, report-all-tagged, trim, explicit.
    #[serde(default)]
    pub with_defaults: Option<String>,
    #[serde(flatten)]
    pub output: OutputFileArg,
}

/// Raw RPC arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RpcArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// RPC body without the `<rpc>` wrapper. A full `<rpc>` document is also accepted.
    pub rpc: String,
    #[serde(flatten)]
    pub output: OutputFileArg,
}

/// `<edit-config>` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditConfigArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Config XML. Include the `<config>` wrapper and namespaces.
    pub config: String,
    /// Target datastore. Default: candidate if advertised, else running.
    #[serde(default)]
    pub target: Option<String>,
    /// Default operation: merge (default), replace, none.
    #[serde(default)]
    pub default_operation: Option<String>,
    /// Test option: test-then-set, set, test-only.
    #[serde(default)]
    pub test_option: Option<String>,
    /// Use a persist confirmed-commit. Confirm later with the commit tool.
    #[serde(default)]
    pub confirmed: bool,
    /// Seconds before auto-rollback. Only used when confirmed is true.
    #[serde(default)]
    pub confirm_timeout: Option<u32>,
    /// Copy running to startup after a normal commit. Default false.
    #[serde(default)]
    pub copy_to_startup: bool,
}

/// `<copy-config>` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CopyConfigArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Source datastore (running, candidate, startup) or URL.
    pub source: String,
    /// Target datastore (running, candidate, startup) or URL.
    pub target: String,
}

/// Confirm / cancel a persist confirmed-commit.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CommitArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Persist id returned by edit_config(confirmed=true).
    pub persist_id: String,
    /// Cancel and roll back instead of confirming.
    #[serde(default)]
    pub cancel: bool,
}

/// `<get-schema>` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetSchemaArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Schema identifier from list_schemas.
    pub identifier: String,
    /// Schema version.
    #[serde(default)]
    pub version: Option<String>,
    /// Modeling language. Default yang.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(flatten)]
    pub output: OutputFileArg,
}

/// `subscribe` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SubscribeArgs {
    #[serde(flatten)]
    pub connection: ConnectionArgs,
    /// Stream name. Default NETCONF.
    #[serde(default)]
    pub stream: Option<String>,
    /// Subtree filter XML. Do not include a `<filter>` wrapper.
    #[serde(default)]
    pub filter: Option<String>,
    /// Replay startTime (RFC 3339).
    #[serde(default)]
    pub start_time: Option<String>,
    /// Replay stopTime (RFC 3339). Requires start_time.
    #[serde(default)]
    pub stop_time: Option<String>,
}

/// `notification_pull` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotificationPullArgs {
    /// Id returned by subscribe.
    pub subscription_id: String,
    /// Milliseconds to wait for at least one notification. 0 = drain only.
    /// Must be <= 300000 (the idle sweep).
    #[serde(default)]
    pub wait_ms: u64,
}

/// `notification_cancel` arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NotificationCancelArgs {
    /// Id returned by subscribe.
    pub subscription_id: String,
}

/// Generic RPC / get reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct XmlReply {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// edit_config / copy_config / commit reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct OkReply {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One YANG schema list entry.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SchemaInfo {
    pub identifier: String,
    pub version: String,
    pub format: String,
    pub namespace: String,
}

/// list_schemas reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SchemasReply {
    pub success: bool,
    pub schemas: Vec<SchemaInfo>,
}

/// get_schema reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SchemaReply {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// subscribe reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SubscribeReply {
    pub success: bool,
    pub subscription_id: String,
}

/// notification_pull reply.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PullReply {
    pub success: bool,
    pub notifications: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_timeout_rejects_negative_json() {
        let err = serde_json::from_str::<EditConfigArgs>(
            r#"{"host":"192.0.2.1","config":"<config/>","confirm_timeout":-1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("u32"), "{err}");
    }

    #[test]
    fn output_file_flattens_onto_get_args() {
        let args: GetArgs =
            serde_json::from_str(r#"{"host":"192.0.2.1","output_file":"/tmp/g.xml"}"#).unwrap();
        assert_eq!(args.output.output_file.as_deref(), Some("/tmp/g.xml"));
        let args: GetArgs = serde_json::from_str(r#"{"host":"192.0.2.1"}"#).unwrap();
        assert!(args.output.output_file.is_none());
    }

    #[test]
    fn file_replies_omit_inline_body() {
        let xml = XmlReply {
            success: true,
            reply: None,
            file: Some("/tmp/g.xml".into()),
            bytes: Some(4),
        };
        let v = serde_json::to_value(&xml).unwrap();
        assert!(v.get("reply").is_none(), "{v}");
        let schema = SchemaReply {
            success: true,
            schema: None,
            file: Some("/tmp/s.yang".into()),
            bytes: Some(4),
        };
        let v = serde_json::to_value(&schema).unwrap();
        assert!(v.get("schema").is_none(), "{v}");
    }
}
