use crate::commands::builtin::{value_of_if_exists, values_of};
use async_ssh2_lite::{AsyncSession, SessionConfiguration};
use clap::ArgMatches;
use dirs::home_dir;
use log::{debug, error, info, warn};
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use ssh2::MethodType;
use ssh2_config::{DefaultAlgorithms, HostParams, ParseRule, SshConfig};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

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
}

impl CliConfig {
    pub fn new(args: ArgMatches) -> NetconfClientResult<Self> {
        let mut ssh_dir = home_dir().unwrap_or(PathBuf::from("/"));
        ssh_dir.extend(Path::new(".ssh/config"));
        let ssh_config = read_ssh_config(&ssh_dir);
        let hosts = values_of::<String>("host", &args)
            .iter()
            .map(|h| h.to_string())
            .collect();
        let username = value_of_if_exists::<String>("username", &args).cloned();
        let password = value_of_if_exists::<String>("password", &args).cloned();
        Ok(Self {
            inner: Arc::new(Config {
                username,
                password,
                addresses: hosts,
                args,
                ssh_config,
            }),
        })
    }
}

fn read_ssh_config(dir: &Path) -> Option<SshConfig> {
    debug!("Trying to parse ssh configuration '{}'", dir.display());

    let mut reader = match File::open(dir) {
        Ok(f) => BufReader::new(f),
        Err(err) => {
            warn!(
                "Could not open ssh config file '{}', error: {}",
                dir.display(),
                err
            );
            return None;
        }
    };
    match SshConfig::default().parse(&mut reader, ParseRule::ALLOW_UNKNOWN_FIELDS) {
        Ok(config) => {
            debug!("Successfully parsed configuration");
            Some(config)
        }
        Err(err) => {
            error!("Failed to parse ssh configuration, error '{}'", err);
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
        })
    }

    pub(crate) async fn connect_ssh(&self) -> NetconfClientResult<AsyncSession<TcpStream>> {
        let stream = self.tcp_connect_timeout().await?;
        self.handshake_and_auth(stream).await
    }

    async fn handshake_and_auth(
        &self,
        stream: TcpStream,
    ) -> NetconfClientResult<AsyncSession<TcpStream>> {
        let mut configuration = SessionConfiguration::new();
        configuration.set_timeout(10_000);
        if let Some(compress) = &self.params.compression {
            debug!(target: &self.address, "Setting compression: {}", compress);
            configuration.set_compress(*compress);
        }
        if self.params.tcp_keep_alive.unwrap_or(false)
            && let Some(interval) = self.params.server_alive_interval
        {
            let interval = interval.as_secs() as u32;
            debug!(target: &self.address, "Setting keepalive interval: {} seconds", interval);
            configuration.set_keepalive(true, interval);
        }
        let mut session = AsyncSession::new(stream, configuration)?;
        configure_session(&mut session, &self.params).await?;
        session.handshake().await?;

        if let Some(password) = &self.auth_password {
            session.userauth_password(&self.auth_user, password).await?;
        } else {
            let mut agent = session.agent()?;
            agent.connect().await?;
            agent.list_identities().await?;

            for identity in agent.identities().unwrap() {
                debug!(
                    target: &self.address,
                    "Trying authentication with public key '{}'",
                    identity.comment()
                );
                match agent.userauth(&self.auth_user, &identity).await {
                    Ok(_) => break,
                    Err(err) => {
                        debug!(
                            target: &self.address,
                            "Public key '{}' rejected: {}",
                            identity.comment(),
                            err
                        );
                        continue;
                    }
                }
            }
        }

        if !session.authenticated() {
            return Err(NetconfClientError::new(format!(
                "SSH authentication failed for {}@{}:{}",
                self.auth_user, self.address, self.port
            )));
        }
        Ok(session)
    }

    async fn tcp_connect_timeout(&self) -> NetconfClientResult<TcpStream> {
        match self.params.proxy_jump.as_deref() {
            Some(jumps) if !jumps.is_empty() => self.connect_via_proxy_jump(jumps).await,
            _ => self.connect_direct().await,
        }
    }

    async fn connect_direct(&self) -> NetconfClientResult<TcpStream> {
        timeout(
            Duration::from_secs(10),
            TcpStream::connect((self.address.as_str(), self.port)),
        )
        .await
        .map_err(|_| {
            NetconfClientError::new(format!(
                "timeout connecting to {}:{}",
                self.address, self.port
            ))
        })?
        .map_err(Into::into)
    }

    async fn connect_via_proxy_jump(&self, jumps: &[String]) -> NetconfClientResult<TcpStream> {
        if jumps.len() != 1 {
            return Err(NetconfClientError::new(format!(
                "only one ProxyJump hop supported, got: {}",
                jumps.join(",")
            )));
        }

        let jump = self.resolve_jump(&jumps[0])?;
        debug!(
            target: &self.address,
            "Connecting via ProxyJump {} ({}:{})",
            jumps[0], jump.address, jump.port
        );
        let jump_stream = jump.connect_direct().await?;
        let jump_session = jump.handshake_and_auth(jump_stream).await?;
        info!(
            target: &self.address,
            "Connected to proxy {}:{}",
            jump.address, jump.port
        );

        let channel = jump_session
            .channel_direct_tcpip(&self.address, self.port, Some(("127.0.0.1", 22)))
            .await?;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _keep_jump = jump_session;
            match listener.accept().await {
                Ok((mut sock, _)) => {
                    let mut channel = channel;
                    if let Err(err) = tokio::io::copy_bidirectional(&mut channel, &mut sock).await {
                        debug!("ProxyJump tunnel closed: {err}");
                    }
                }
                Err(err) => error!("ProxyJump local accept failed: {err}"),
            }
        });

        timeout(Duration::from_secs(10), TcpStream::connect(local_addr))
            .await
            .map_err(|_| {
                NetconfClientError::new("timeout connecting through ProxyJump".to_string())
            })?
            .map_err(Into::into)
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
        Host::new(
            &spec.host,
            &user,
            &None,
            params,
            22,
            self.ssh_config.clone(),
        )
    }
}
async fn configure_session(
    session: &mut AsyncSession<TcpStream>,
    params: &HostParams,
) -> NetconfClientResult<()> {
    if !params.kex_algorithms.is_default() {
        session
            .method_pref(
                MethodType::Kex,
                params.kex_algorithms.algorithms().join(",").as_str(),
            )
            .await?;
    }
    if !params.host_key_algorithms.is_default() {
        session
            .method_pref(
                MethodType::HostKey,
                params.host_key_algorithms.algorithms().join(",").as_str(),
            )
            .await?;
    }
    if !params.ciphers.is_default() {
        session
            .method_pref(
                MethodType::CryptCs,
                params.ciphers.algorithms().join(",").as_str(),
            )
            .await?;
    }
    if !params.mac.is_default() {
        session
            .method_pref(
                MethodType::MacCs,
                params.mac.algorithms().join(",").as_str(),
            )
            .await?;
        session
            .method_pref(
                MethodType::MacSc,
                params.mac.algorithms().join(",").as_str(),
            )
            .await?;
    }
    Ok(())
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
}
