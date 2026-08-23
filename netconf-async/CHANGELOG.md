# Changelog

## [0.1.0](https://github.com/Mrflatt/netconf-rust/releases/tag/netconf-async-v0.1.0) (2026-08-23)

Initial release.

Async NETCONF client over SSH (`netconf` subsystem). rustc 1.85+, edition 2024. SSH via russh (pure Rust).

* Hello exchange and automatic upgrade from base 1.0 (`]]>]]>`) to 1.1 chunked framing
* RFC 6241 RPCs: get, get-config, edit-config, copy-config, delete-config, lock / unlock, validate, commit / confirmed-commit / confirm-commit / cancel-commit, discard-changes, close-session, kill-session, raw RPC
* RFC 5277 `<create-subscription>` with replay `startTime` / `stopTime`; interleaved notifications buffered during later RPCs
* RFC 6243 `with-defaults`
* SSH: password, agent, or key file; ProxyJump; shared `JumpPool`
* Host-key policy: reject-all (default), fingerprint, known_hosts, accept-new, accept-all
* Per-RPC timeout, raw-XML replies, warning-severity `<rpc-error>` as success by default
* Custom `Transport` for non-SSH pipes
