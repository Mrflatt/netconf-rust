# Changelog

## [0.2.0](https://github.com/Mrflatt/netconf-rust/compare/netconf-cli-v0.1.0...netconf-cli-v0.2.0) (2026-08-29)


### Features

* honor ssh_config IdentitiesOnly ([c956ae3](https://github.com/Mrflatt/netconf-rust/commit/c956ae3bde93341c6bbd711668c94d6fe5ec81d6))
* keyboard-interactive SSH fallback and pretty XML by default ([f16e920](https://github.com/Mrflatt/netconf-rust/commit/f16e92002ff29d8321e8ffa8f980aa69a168c234))


### Miscellaneous Chores

* **deps:** bump sha2 in the major-dependencies group ([#27](https://github.com/Mrflatt/netconf-rust/issues/27)) ([1492c69](https://github.com/Mrflatt/netconf-rust/commit/1492c69bb34f898f22b7b189ff906a50efa8f723))


### Dependencies

* The following workspace dependencies were updated
  * dependencies
    * netconf-async bumped from 0.1.0 to 0.2.0

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
