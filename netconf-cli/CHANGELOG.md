# Changelog

## [0.1.0](https://github.com/Mrflatt/netconf-rust/releases/tag/netconf-cli-v0.1.0) (2026-08-22)

Initial release.

CLI over `netconf-async`. Parallel multi-host runs, OpenSSH config, ssh-agent, ProxyJump.

* Commands: get, get-config, edit, copy, commit, rpc, notification, update
* `--filter` / `--file`: inline XML, `@path`, file, or `-` (stdin)
* `--timeout` per RPC; `--parallel` concurrent hosts (default: CPU count)
* Shared ProxyJump session across hosts on the same hop chain
* `--format` and `--output-dir`
* `netconf-cli update` self-replaces from GitHub releases
