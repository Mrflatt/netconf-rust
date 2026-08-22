# Changelog

## [0.1.0](https://github.com/Mrflatt/netconf-rust/compare/netconf-cli-v0.1.0...netconf-cli-v0.1.0) (2026-08-22)


### ⚠ BREAKING CHANGES

* accept inline XML and RFC 5277 replay times
* **netconf-async:** replace libssh2 with russh
* **netconf-async:** own SSH connect and harden RPC replies
* **netconf-async:** session_id returns Option<u64>; set_skip_serializing becomes set_parse_replies; RpcReply::get_message_id becomes message_id; the NetconfClientError::Anyhow variant is replaced by Other; the async-ssh2-lite and async-trait features are replaced by ssh.

### Features

* accept inline XML and RFC 5277 replay times ([2f0f59d](https://github.com/Mrflatt/netconf-rust/commit/2f0f59d184923daf7d96f9e032cc13c21c02e84b))
* add remaining RFC 6241 ops and edit/copy/commit/rpc CLI ([38f5487](https://github.com/Mrflatt/netconf-rust/commit/38f5487fd46a7778efb91dc6e4048c65be4a0faa))
* **cli:** add GitHub release poller and self-update ([#13](https://github.com/Mrflatt/netconf-rust/issues/13)) ([fb504b6](https://github.com/Mrflatt/netconf-rust/commit/fb504b6eb2887426b9adc996fd494e4e3f103223))
* **cli:** connect through ssh_config ProxyJump ([8547bae](https://github.com/Mrflatt/netconf-rust/commit/8547bae20fcadd7b9bb1183268ab21546e742969))
* **cli:** send replies to stdout and add --format/--output-dir ([4d33a52](https://github.com/Mrflatt/netconf-rust/commit/4d33a529b1a9cd0170ee553924e3318869fbba26))
* multiplex ProxyJump sessions across fan-out hosts ([b6bfa80](https://github.com/Mrflatt/netconf-rust/commit/b6bfa80c275eba1340bf9c236028b3f1402c2e05))
* **netconf-async:** own SSH connect and harden RPC replies ([76cbab8](https://github.com/Mrflatt/netconf-rust/commit/76cbab890df385befdf956bee4695ad4addfac55))
* **netconf-async:** replace libssh2 with russh ([7846448](https://github.com/Mrflatt/netconf-rust/commit/7846448add93cfdf20ff027f739cb559cadea2f3))


### Bug Fixes

* **cli:** honor filter files on get, get-config, and notification ([045dfb9](https://github.com/Mrflatt/netconf-rust/commit/045dfb9e5e7230f695dc63c94ce1f518a0659920))
* **cli:** raise the libssh2 timeout, and guard features and MSRV in CI ([#22](https://github.com/Mrflatt/netconf-rust/issues/22)) ([6fe49cc](https://github.com/Mrflatt/netconf-rust/commit/6fe49cc5890d1722185015341c8e067f21ed6993))
* **netconf-async:** harden framing, reply parsing, and session teardown ([#21](https://github.com/Mrflatt/netconf-rust/issues/21)) ([70d708e](https://github.com/Mrflatt/netconf-rust/commit/70d708e954f13bc914fb941c4d47f0ad055c68cc))


### Miscellaneous Chores

* bump rust edition and dependencies ([3d86054](https://github.com/Mrflatt/netconf-rust/commit/3d86054860bae83fbb647db6c67c51b80cfb54ac))
