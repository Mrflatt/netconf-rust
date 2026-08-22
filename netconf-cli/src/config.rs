use crate::commands::builtin::{value_of_if_exists, values_of};
use clap::ArgMatches;
use dirs::home_dir;
use log::{debug, error, warn};
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::transport::ssh::{
    HostKeyPolicy, SshAuth, SshConfig as TransportSshConfig, SshJump, SshSessionOpts,
};
use ssh2_config::{DefaultAlgorithms, HostParams, ParseRule, SshConfig};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CliConfig {
    pub inner: Arc<Config>,
}

#[derive(Debug)]
pub struct Config {
    pub args: ArgMatches,
    pub ssh_config: Option<SshConfig>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub addresses: Vec<String>,
    pub strict_host_key: Option<HostKeyCheck>,
    pub timeout: Option<Duration>,
    pub parallel: Option<usize>,
    pub stdin_xml: Option<String>,
}

impl CliConfig {
    pub fn new(args: ArgMatches) -> NetconfClientResult<Self> {
        let ssh_config = load_ssh_config();
        let hosts = values_of::<String>("host", &args)
            .iter()
            .map(|h| h.to_string())
            .collect();
        let username = value_of_if_exists::<String>("username", &args).cloned();
        let password = value_of_if_exists::<String>("password", &args).cloned();
        let strict_host_key = value_of_if_exists::<String>("strict-host-key-checking", &args)
            .and_then(|value| parse_host_key_check(value));
        let timeout =
            value_of_if_exists::<u64>("timeout", &args).map(|&secs| Duration::from_secs(secs));
        let parallel = value_of_if_exists::<u64>("parallel", &args).map(|&n| n as usize);
        let stdin_xml = capture_stdin_xml(&args)?;
        Ok(Self {
            inner: Arc::new(Config {
                username,
                password,
                addresses: hosts,
                args,
                ssh_config,
                strict_host_key,
                timeout,
                parallel,
                stdin_xml,
            }),
        })
    }
}

const SYSTEM_SSH_CONFIG: &str = "/etc/ssh/ssh_config";

fn user_ssh_config_path() -> PathBuf {
    let mut path = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    path.push(".ssh/config");
    path
}

fn load_ssh_config() -> Option<SshConfig> {
    merge_ssh_configs(
        read_ssh_config(&user_ssh_config_path()),
        read_ssh_config(Path::new(SYSTEM_SSH_CONFIG)),
    )
}

fn merge_ssh_configs(
    preferred: Option<SshConfig>,
    fallback: Option<SshConfig>,
) -> Option<SshConfig> {
    match (preferred, fallback) {
        (None, None) => None,
        (Some(config), None) | (None, Some(config)) => Some(config),
        (Some(preferred), Some(fallback)) => {
            let mut hosts = preferred.get_hosts().clone();
            hosts.extend(fallback.get_hosts().iter().cloned());
            Some(SshConfig::from_hosts(hosts))
        }
    }
}

fn capture_stdin_xml(args: &ArgMatches) -> NetconfClientResult<Option<String>> {
    let needs_stdin = ["filter", "file"].iter().any(|name| {
        args.try_get_one::<String>(name)
            .ok()
            .flatten()
            .is_some_and(|value| value == "-")
    });
    if !needs_stdin {
        return Ok(None);
    }
    let mut content = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut content)
        .map_err(|err| NetconfClientError::new(format!("failed to read stdin: {err}")))?;
    Ok(Some(content))
}

fn read_ssh_config(path: &Path) -> Option<SshConfig> {
    debug!("Trying to parse ssh configuration '{}'", path.display());

    let mut reader = match File::open(path) {
        Ok(f) => BufReader::new(f),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!("No ssh config at '{}'", path.display());
            return None;
        }
        Err(err) => {
            warn!(
                "Could not open ssh config file '{}', error: {}",
                path.display(),
                err
            );
            return None;
        }
    };
    match SshConfig::default().parse(
        &mut reader,
        ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
    ) {
        Ok(config) => {
            debug!("Successfully parsed configuration {}", path.display());
            Some(config)
        }
        Err(err) => {
            error!(
                "Failed to parse ssh configuration '{}', error '{}'",
                path.display(),
                err
            );
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JumpSpec {
    user: Option<String>,
    host: String,
    port: Option<u16>,
}

fn split_host_port(addr: &str) -> NetconfClientResult<(String, Option<u16>)> {
    if let Some(rest) = addr.strip_prefix('[') {
        let Some((host, after)) = rest.split_once(']') else {
            return Err(NetconfClientError::new(format!(
                "unclosed '[' in address '{addr}'"
            )));
        };
        if host.is_empty() {
            return Err(NetconfClientError::new(format!(
                "empty address in '{addr}'"
            )));
        }
        let port = match after {
            "" => None,
            s => {
                let Some(port) = s.strip_prefix(':') else {
                    return Err(NetconfClientError::new(format!("invalid address '{addr}'")));
                };
                Some(parse_port(port, addr)?)
            }
        };
        return Ok((host.to_string(), port));
    }

    // Bare IPv6 has more than one ':'. Port is only allowed with brackets.
    if addr.matches(':').count() > 1 {
        return Ok((addr.to_string(), None));
    }

    match addr.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            Ok((host.to_string(), Some(parse_port(port, addr)?)))
        }
        _ => Ok((addr.to_string(), None)),
    }
}

fn parse_port(port: &str, addr: &str) -> NetconfClientResult<u16> {
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(NetconfClientError::new(format!("invalid port in '{addr}'")));
    }
    port.parse::<u16>()
        .map_err(|err| NetconfClientError::new(format!("invalid port in '{addr}': {err}")))
}

fn parse_jump_spec(raw: &str) -> NetconfClientResult<JumpSpec> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(NetconfClientError::new("empty ProxyJump host".to_string()));
    }
    let (user, hostport) = match raw.split_once('@') {
        Some((user, rest)) if !user.is_empty() && !rest.is_empty() => {
            (Some(user.to_string()), rest)
        }
        Some(_) => {
            return Err(NetconfClientError::new(format!(
                "invalid ProxyJump spec '{raw}'"
            )));
        }
        None => (None, raw),
    };
    let (host, port) = split_host_port(hostport)?;
    Ok(JumpSpec { user, host, port })
}

#[derive(Debug)]
pub struct Host {
    pub(crate) address: String,
    port: u16,
    auth_user: String,
    auth_password: Option<String>,
    params: HostParams,
    ssh_config: Option<SshConfig>,
    strict_host_key: Option<HostKeyCheck>,
}

impl Host {
    pub(crate) fn new(
        addr: &str,
        username: &Option<String>,
        password: &Option<String>,
        params: HostParams,
        default_port: u16,
        ssh_config: Option<SshConfig>,
    ) -> NetconfClientResult<Host> {
        let (host_part, explicit_port) = split_host_port(addr)?;

        let address = params.host_name.clone().unwrap_or(host_part);
        let port = explicit_port.or(params.port).unwrap_or(default_port);

        let auth_user = if let Some(user) = username {
            user.clone()
        } else if let Some(user) = params.user.as_deref() {
            user.to_string()
        } else {
            return Err(NetconfClientError::new("No username provided".to_string()));
        };

        Ok(Host {
            address,
            port,
            params,
            auth_user,
            auth_password: password.clone(),
            ssh_config,
            strict_host_key: None,
        })
    }

    /// CLI / ssh_config override for host-key checking. `None` means default AcceptNew.
    pub(crate) fn strict_host_key(mut self, value: Option<HostKeyCheck>) -> Self {
        self.strict_host_key = value;
        self
    }

    /// Library SSH config. Host keys default to [`HostKeyPolicy::AcceptNew`].
    /// Jump auth is IdentityFile or agent — the device password is not reused.
    pub(crate) fn ssh_transport_config(&self) -> NetconfClientResult<TransportSshConfig> {
        let mut config = TransportSshConfig::new(
            &self.address,
            self.port,
            &self.auth_user,
            ssh_auth(&self.auth_password, &self.params),
        )
        .host_key(host_key_policy(&self.params, self.strict_host_key))
        .session(session_opts(&self.params));
        if let Some(jumps) = self.params.proxy_jump.as_deref() {
            for spec in jumps {
                let jump = self.resolve_jump(spec)?;
                debug!(
                    target: &self.address,
                    "Connecting via ProxyJump {spec} ({}:{})",
                    jump.address, jump.port
                );
                config = config.jump(
                    SshJump::new(
                        &jump.address,
                        jump.port,
                        &jump.auth_user,
                        ssh_auth(&None, &jump.params),
                    )
                    .host_key(host_key_policy(&jump.params, jump.strict_host_key))
                    .session(session_opts(&jump.params)),
                );
            }
        }
        Ok(config)
    }

    fn resolve_jump(&self, spec: &str) -> NetconfClientResult<Host> {
        let spec = parse_jump_spec(spec)?;
        let mut params = match &self.ssh_config {
            Some(cfg) => cfg.query(&spec.host),
            None => HostParams::new(&DefaultAlgorithms::default()),
        };
        if spec.port.is_some() {
            params.port = spec.port;
        }
        let user = spec.user.or(params.user.clone());
        Ok(Host::new(
            &spec.host,
            &user,
            &None,
            params,
            22,
            self.ssh_config.clone(),
        )?
        .strict_host_key(self.strict_host_key))
    }
}

fn ssh_auth(password: &Option<String>, params: &HostParams) -> SshAuth {
    if let Some(password) = password {
        return SshAuth::password(password.clone());
    }
    if let Some(files) = params.identity_file.as_ref()
        && let Some(path) = files.first()
    {
        if files.len() > 1 {
            debug!(
                "using first IdentityFile {}; extra files are ignored",
                path.display()
            );
        }
        return SshAuth::key_file(expand_tilde(path), None);
    }
    SshAuth::Agent
}

/// How the CLI should treat SSH host keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostKeyCheck {
    /// Accept any key.
    Off,
    /// Reject unknown hosts and changed keys.
    Strict,
    /// Accept unknown hosts and pin them; reject changed keys.
    AcceptNew,
}

fn host_key_policy(params: &HostParams, strict: Option<HostKeyCheck>) -> HostKeyPolicy {
    match strict
        .or_else(|| ssh_config_host_key_check(params))
        .unwrap_or(HostKeyCheck::AcceptNew)
    {
        HostKeyCheck::Off => HostKeyPolicy::AcceptAll,
        HostKeyCheck::Strict => HostKeyPolicy::KnownHosts(known_hosts_path(params)),
        HostKeyCheck::AcceptNew => HostKeyPolicy::AcceptNew(known_hosts_path(params)),
    }
}

fn ssh_config_host_key_check(params: &HostParams) -> Option<HostKeyCheck> {
    params
        .unsupported_fields
        .get("stricthostkeychecking")
        .and_then(|values| values.first())
        .and_then(|value| parse_host_key_check(value))
}

fn parse_host_key_check(value: &str) -> Option<HostKeyCheck> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "ask" => Some(HostKeyCheck::Strict),
        "accept-new" | "acceptnew" => Some(HostKeyCheck::AcceptNew),
        "no" | "false" | "off" => Some(HostKeyCheck::Off),
        _ => None,
    }
}

fn known_hosts_path(params: &HostParams) -> PathBuf {
    if let Some(paths) = params.unsupported_fields.get("userknownhostsfile")
        && let Some(raw) = paths.first()
        && let Some(first) = raw.split_whitespace().next()
    {
        return expand_tilde(Path::new(first));
    }
    default_known_hosts_path()
}

fn default_known_hosts_path() -> PathBuf {
    let mut path = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    path.push(".ssh/known_hosts");
    path
}

fn session_opts(params: &HostParams) -> SshSessionOpts {
    let mut opts = SshSessionOpts::new();
    if let Some(compress) = params.compression {
        opts = opts.compression(compress);
    }
    if params.tcp_keep_alive.unwrap_or(false)
        && let Some(interval) = params.server_alive_interval
    {
        opts = opts.keepalive(interval);
    }
    if !params.kex_algorithms.is_default() {
        opts = opts.kex_algorithms(params.kex_algorithms.algorithms().join(","));
    }
    if !params.host_key_algorithms.is_default() {
        opts = opts.host_key_algorithms(params.host_key_algorithms.algorithms().join(","));
    }
    if !params.ciphers.is_default() {
        opts = opts.ciphers(params.ciphers.algorithms().join(","));
    }
    if !params.mac.is_default() {
        opts = opts.macs(params.mac.algorithms().join(","));
    }
    opts
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let mut home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.push(rest);
        return home;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jump_spec_variants() {
        assert_eq!(
            parse_jump_spec("jump").unwrap(),
            JumpSpec {
                user: None,
                host: "jump".into(),
                port: None,
            }
        );
        assert_eq!(
            parse_jump_spec("jump-user@jump").unwrap(),
            JumpSpec {
                user: Some("jump-user".into()),
                host: "jump".into(),
                port: None,
            }
        );
        assert_eq!(
            parse_jump_spec("jump:2222").unwrap(),
            JumpSpec {
                user: None,
                host: "jump".into(),
                port: Some(2222),
            }
        );
        assert_eq!(
            parse_jump_spec("user@jump.example:2200").unwrap(),
            JumpSpec {
                user: Some("user".into()),
                host: "jump.example".into(),
                port: Some(2200),
            }
        );
        assert!(parse_jump_spec("").is_err());
        assert!(parse_jump_spec("@host").is_err());
        assert_eq!(
            parse_jump_spec("2001:db8::1").unwrap(),
            JumpSpec {
                user: None,
                host: "2001:db8::1".into(),
                port: None,
            }
        );
        assert_eq!(
            parse_jump_spec("[2001:db8::1]:2200").unwrap(),
            JumpSpec {
                user: None,
                host: "2001:db8::1".into(),
                port: Some(2200),
            }
        );
        assert_eq!(
            parse_jump_spec("jump-user@[2001:db8::1]:22").unwrap(),
            JumpSpec {
                user: Some("jump-user".into()),
                host: "2001:db8::1".into(),
                port: Some(22),
            }
        );
        assert!(parse_jump_spec("[2001:db8::1").is_err());
    }

    #[test]
    fn split_host_port_ipv4_ipv6_and_name() {
        assert_eq!(
            split_host_port("192.0.2.10:830").unwrap(),
            ("192.0.2.10".into(), Some(830))
        );
        assert_eq!(
            split_host_port("192.0.2.10").unwrap(),
            ("192.0.2.10".into(), None)
        );
        assert_eq!(
            split_host_port("2001:db8::1").unwrap(),
            ("2001:db8::1".into(), None)
        );
        assert_eq!(
            split_host_port("[2001:db8::1]:830").unwrap(),
            ("2001:db8::1".into(), Some(830))
        );
        assert_eq!(
            split_host_port("[2001:db8::1]").unwrap(),
            ("2001:db8::1".into(), None)
        );
        assert_eq!(split_host_port("::1").unwrap(), ("::1".into(), None));
        assert!(split_host_port("[2001:db8::1:830").is_err());
        assert!(split_host_port("host:").is_err());
    }

    #[test]
    fn host_uses_hostname_and_config_port() {
        let mut params = HostParams::new(&DefaultAlgorithms::default());
        params.host_name = Some("jump.example.test".into());
        params.port = Some(22);
        params.user = Some("jump-user".into());
        let host = Host::new("jump", &None, &None, params, 22, None).unwrap();
        assert_eq!(host.address, "jump.example.test");
        assert_eq!(host.port, 22);
        assert_eq!(host.auth_user, "jump-user");
        assert!(host.auth_password.is_none());
    }

    #[test]
    fn host_prefers_explicit_port_and_cli_user() {
        let mut params = HostParams::new(&DefaultAlgorithms::default());
        params.port = Some(22);
        params.user = Some("from-config".into());
        let host = Host::new(
            "192.0.2.10:830",
            &Some("netconf-user".into()),
            &Some("secret".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert_eq!(host.address, "192.0.2.10");
        assert_eq!(host.port, 830);
        assert_eq!(host.auth_user, "netconf-user");
        assert_eq!(host.auth_password.as_deref(), Some("secret"));
    }

    #[test]
    fn netconf_default_port_when_unspecified() {
        let params = HostParams::new(&DefaultAlgorithms::default());
        let host = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert_eq!(host.port, 830);
    }

    #[test]
    fn host_accepts_ipv6_with_and_without_port() {
        let params = HostParams::new(&DefaultAlgorithms::default());
        let host = Host::new(
            "[2001:db8::10]:830",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert_eq!(host.address, "2001:db8::10");
        assert_eq!(host.port, 830);

        let params = HostParams::new(&DefaultAlgorithms::default());
        let host = Host::new(
            "2001:db8::10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert_eq!(host.address, "2001:db8::10");
        assert_eq!(host.port, 830);
    }

    #[test]
    fn resolve_jump_uses_ssh_config_and_ignores_device_password() {
        let config = SshConfig::default();
        let mut device_params = HostParams::new(&DefaultAlgorithms::default());
        device_params.proxy_jump = Some(vec!["jump".into()]);
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &Some("device-secret".into()),
            device_params,
            830,
            Some(config),
        )
        .unwrap();

        let jump = device.resolve_jump("jump-user@jump:22").unwrap();
        assert_eq!(jump.address, "jump");
        assert_eq!(jump.port, 22);
        assert_eq!(jump.auth_user, "jump-user");
        assert!(jump.auth_password.is_none());
    }

    #[test]
    fn resolve_jump_keeps_port_and_hostname_from_ssh_config() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new(
                    "Host jump\n  HostName jump.example.test\n  Port 2222\n  User jump-user\n",
                ),
                ParseRule::STRICT,
            )
            .unwrap();
        let mut device_params = HostParams::new(&DefaultAlgorithms::default());
        device_params.proxy_jump = Some(vec!["jump".into()]);
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &Some("device-secret".into()),
            device_params,
            830,
            Some(config),
        )
        .unwrap();

        let jump = device.resolve_jump("jump").unwrap();
        assert_eq!(jump.address, "jump.example.test");
        assert_eq!(jump.port, 2222);
        assert_eq!(jump.auth_user, "jump-user");

        let jump = device.resolve_jump("jump:2200").unwrap();
        assert_eq!(jump.port, 2200);
    }

    #[test]
    fn ssh_transport_config_maps_password_and_agent_jump() {
        let mut device_params = HostParams::new(&DefaultAlgorithms::default());
        device_params.proxy_jump = Some(vec!["jump-user@jump:22".into()]);
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &Some("device-secret".into()),
            device_params,
            830,
            None,
        )
        .unwrap();

        let cfg = device.ssh_transport_config().unwrap();
        assert_eq!(cfg.host(), "192.0.2.10");
        assert_eq!(cfg.port(), 830);
        assert_eq!(cfg.username(), "netconf-user");
        assert!(matches!(cfg.auth(), SshAuth::Password(_)));
        assert!(matches!(cfg.host_key_policy(), HostKeyPolicy::AcceptNew(_)));
        let jump = &cfg.jumps()[0];
        assert_eq!(jump.host(), "jump");
        assert_eq!(jump.port(), 22);
        assert_eq!(jump.username(), "jump-user");
        assert_eq!(jump.auth(), &SshAuth::Agent);
        assert!(matches!(
            jump.host_key_policy(),
            HostKeyPolicy::AcceptNew(_)
        ));
    }

    #[test]
    fn ssh_transport_config_maps_multi_hop_proxy_jump() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new(
                    "Host jump1\n  HostName j1.example\n  User u1\n  Port 22\n\
                     Host jump2\n  HostName j2.example\n  User u2\n  Port 2222\n",
                ),
                ParseRule::STRICT,
            )
            .unwrap();
        let mut device_params = HostParams::new(&DefaultAlgorithms::default());
        device_params.proxy_jump = Some(vec!["jump1".into(), "jump2".into()]);
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &Some("device-secret".into()),
            device_params,
            830,
            Some(config),
        )
        .unwrap();

        let hops = device.ssh_transport_config().unwrap().jumps().to_vec();
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].host(), "j1.example");
        assert_eq!(hops[0].username(), "u1");
        assert_eq!(hops[0].port(), 22);
        assert_eq!(hops[1].host(), "j2.example");
        assert_eq!(hops[1].username(), "u2");
        assert_eq!(hops[1].port(), 2222);
        assert!(hops.iter().all(|h| matches!(h.auth(), SshAuth::Agent)));
    }

    #[test]
    fn ssh_transport_config_maps_identity_file_and_session_opts() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new(
                    "Host *\n\
                     IdentityFile ~/.ssh/id_ed25519\n\
                     Compression yes\n\
                     TCPKeepAlive yes\n\
                     ServerAliveInterval 15\n\
                     Ciphers aes256-ctr\n\
                     KexAlgorithms curve25519-sha256\n\
                     MACs hmac-sha2-256\n\
                     HostKeyAlgorithms ssh-ed25519\n",
                ),
                ParseRule::STRICT,
            )
            .unwrap();
        let params = config.query("192.0.2.10");
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &None,
            params,
            830,
            Some(config),
        )
        .unwrap();

        let cfg = device.ssh_transport_config().unwrap();
        match cfg.auth() {
            SshAuth::KeyFile { path, passphrase } => {
                assert!(path.ends_with(".ssh/id_ed25519"), "{path:?}");
                assert!(passphrase.is_none());
            }
            other => panic!("expected KeyFile, got {other:?}"),
        }
        assert_eq!(cfg.session_opts().compression_enabled(), Some(true));
        assert_eq!(
            cfg.session_opts().keepalive_interval(),
            Some(std::time::Duration::from_secs(15))
        );
        assert_eq!(cfg.session_opts().ciphers_pref(), Some("aes256-ctr"));
        assert_eq!(cfg.session_opts().kex(), Some("curve25519-sha256"));
        assert_eq!(cfg.session_opts().macs_pref(), Some("hmac-sha2-256"));
        assert_eq!(cfg.session_opts().host_key_algs(), Some("ssh-ed25519"));
    }

    #[test]
    fn password_wins_over_identity_file() {
        let mut params = HostParams::new(&DefaultAlgorithms::default());
        params.identity_file = Some(vec![PathBuf::from("/tmp/id")]);
        let device = Host::new(
            "192.0.2.10",
            &Some("netconf-user".into()),
            &Some("device-secret".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert!(matches!(
            device.ssh_transport_config().unwrap().auth(),
            SshAuth::Password(_)
        ));
    }

    #[test]
    fn host_keys_default_to_accept_new() {
        let params = HostParams::new(&DefaultAlgorithms::default());
        let device = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            None,
        )
        .unwrap();
        assert!(matches!(
            device.ssh_transport_config().unwrap().host_key_policy(),
            HostKeyPolicy::AcceptNew(_)
        ));
    }

    #[test]
    fn ssh_config_can_disable_host_key_checking() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new("Host *\n  StrictHostKeyChecking no\n"),
                ParseRule::ALLOW_UNSUPPORTED_FIELDS,
            )
            .unwrap();
        let params = config.query("192.0.2.10");
        let device = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            Some(config),
        )
        .unwrap();
        assert_eq!(
            device.ssh_transport_config().unwrap().host_key_policy(),
            &HostKeyPolicy::AcceptAll
        );
    }

    #[test]
    fn strict_host_key_checking_uses_known_hosts() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new(
                    "Host *\n  StrictHostKeyChecking yes\n  UserKnownHostsFile /tmp/kh\n",
                ),
                ParseRule::ALLOW_UNSUPPORTED_FIELDS,
            )
            .unwrap();
        let params = config.query("192.0.2.10");
        let device = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            Some(config),
        )
        .unwrap();
        assert_eq!(
            device.ssh_transport_config().unwrap().host_key_policy(),
            &HostKeyPolicy::KnownHosts(PathBuf::from("/tmp/kh"))
        );
    }

    #[test]
    fn accept_new_maps_to_tofu_policy() {
        let config = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new("Host *\n  StrictHostKeyChecking accept-new\n"),
                ParseRule::ALLOW_UNSUPPORTED_FIELDS,
            )
            .unwrap();
        let params = config.query("192.0.2.10");
        let device = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            Some(config),
        )
        .unwrap();
        assert!(matches!(
            device.ssh_transport_config().unwrap().host_key_policy(),
            HostKeyPolicy::AcceptNew(_)
        ));
    }

    #[test]
    fn cli_strict_host_key_override_wins() {
        let params = HostParams::new(&DefaultAlgorithms::default());
        let device = Host::new(
            "192.0.2.10",
            &Some("u".into()),
            &Some("p".into()),
            params,
            830,
            None,
        )
        .unwrap()
        .strict_host_key(Some(HostKeyCheck::Strict));
        assert!(matches!(
            device.ssh_transport_config().unwrap().host_key_policy(),
            HostKeyPolicy::KnownHosts(_)
        ));
    }

    #[test]
    fn expand_tilde_rewrites_home() {
        let expanded = expand_tilde(Path::new("~/.ssh/id_ed25519"));
        assert!(!expanded.starts_with("~"), "{expanded:?}");
        assert!(expanded.ends_with(".ssh/id_ed25519"), "{expanded:?}");
    }

    #[test]
    fn merge_ssh_configs_user_wins_system_fills_gaps() {
        let user = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new("Host foo\n  User alice\n"),
                ParseRule::STRICT,
            )
            .unwrap();
        let system = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new("Host foo\n  User bob\n  Port 2222\n"),
                ParseRule::STRICT,
            )
            .unwrap();

        let merged = merge_ssh_configs(Some(user), Some(system)).unwrap();
        let params = merged.query("foo");
        assert_eq!(params.user.as_deref(), Some("alice"));
        assert_eq!(params.port, Some(2222));
    }

    #[test]
    fn merge_ssh_configs_falls_back_when_user_missing() {
        let system = SshConfig::default()
            .parse(
                &mut std::io::Cursor::new("Host *\n  User sysuser\n"),
                ParseRule::STRICT,
            )
            .unwrap();
        let merged = merge_ssh_configs(None, Some(system)).unwrap();
        assert_eq!(merged.query("foo").user.as_deref(), Some("sysuser"));
    }
}
