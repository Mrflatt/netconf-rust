//! # netconf-async
//!
//! ```toml
//! netconf-async = "^0.2.0"
//! ```
//!
//! ## Example
//!
//! Here is a basic example:
//!
//! ```rust
//! ```
//!
pub mod connection;
pub mod error;
pub mod framer;
pub mod message;
pub mod transport;

pub const NETCONF_URN: &str = "urn:ietf:params:xml:ns:netconf:base:1.0";
pub const NETCONF_BASE_10_CAP: &str = "urn:ietf:params:netconf:base:1.0";
pub const NETCONF_BASE_11_CAP: &str = "urn:ietf:params:netconf:base:1.1";
pub const WRITABLE_RUNNING_CAP: &str = "urn:ietf:params:netconf:capability:writable-running:1.0";
pub const CANDIDATE_CAP: &str = "urn:ietf:params:netconf:capability:candidate:1.0";
pub const STARTUP_CAP: &str = "urn:ietf:params:netconf:capability:startup:1.0";
pub const ROLLBACK_ON_ERROR_CAP: &str = "urn:ietf:params:netconf:capability:rollback-on-error:1.0";
pub const URL_CAP: &str = "urn:ietf:params:netconf:capability:url:1.0";
pub const CONFIRMED_COMMIT_10_CAP: &str = "urn:ietf:params:netconf:capability:confirmed-commit:1.0";
pub const CONFIRMED_COMMIT_CAP: &str = "urn:ietf:params:netconf:capability:confirmed-commit:1.1";
pub const VALIDATE_10_CAP: &str = "urn:ietf:params:netconf:capability:validate:1.0";
pub const VALIDATE_CAP: &str = "urn:ietf:params:netconf:capability:validate:1.1";
pub const WITH_DEFAULTS_CAP: &str = "urn:ietf:params:netconf:capability:with-defaults:1.0";
pub const NOTIFICATION_CAP: &str = "urn:ietf:params:netconf:capability:notification:1.0";
