# Changelog

## [0.1.0](https://github.com/Mrflatt/netconf-rust/releases/tag/netconf-cli-v0.1.0) (2026-08-23)

Initial release.

CLI over `netconf-async`. Parallel multi-host runs, OpenSSH config, ssh-agent, ProxyJump.

* Commands: get, get-config, edit, copy, commit, rpc, notification, update
* `--filter` / `--file`: inline XML, `@path`, file, directory (name order), or `-` (stdin)
* Inventory `--host @file.csv` plus Go-template subset; `--dry-run` renders without SSH
* `--timeout` per RPC; `--parallel` concurrent hosts (default: CPU count)
* Shared ProxyJump session across hosts on the same hop chain
* `--format` (xml / json, pretty, unescape) and `--output-dir`
* Host keys default to accept-new; `--strict-host-key-checking yes|accept-new|no`
* `netconf-cli update` self-replaces from GitHub releases
