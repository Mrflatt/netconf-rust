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
