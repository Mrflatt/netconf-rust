use crate::commands::builtin::{value_of_if_exists, values_of};
use async_ssh2_lite::{AsyncSession, SessionConfiguration};
use clap::ArgMatches;
use dirs::home_dir;
use log::{debug, error, warn};
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use ssh2::MethodType;
use ssh2_config::{HostParams, ParseRule, SshConfig};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
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

#[derive(Debug)]
pub struct Host {
    pub(crate) address: String,
    port: u16,
    auth_user: String,
    auth_password: Option<String>,
    params: HostParams,
}

impl Host {
    pub(crate) fn new(
        addr: &str,
        username: &Option<String>,
        password: &Option<String>,
        params: HostParams,
    ) -> NetconfClientResult<Host> {
        let (address, port) = match addr.split_once(':') {
            Some((address, port)) => (address.to_string(), port.parse().unwrap()),
            None => (addr.to_string(), 830),
        };

        let auth_user = if let Some(user) = username {
            user.clone()
        } else if let Some(user) = params.user.as_deref() {
            user.to_string()
        } else {
            return Err(NetconfClientError::new("No username provided".to_string()));
        };

        let auth_password = if password.is_some() {
            password.clone()
        } else if params.identity_file.is_none() {
            return Err(NetconfClientError::new(
                "No password or identity file provided".to_string(),
            ));
        } else {
            None
        };

        Ok(Host {
            address,
            port,
            params,
            auth_user,
            auth_password,
        })
    }

    pub(crate) async fn connect_ssh(&self) -> NetconfClientResult<AsyncSession<TcpStream>> {
        let stream: TcpStream = self.tcp_connect_timeout().await?;
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
            Ok(session)
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
                        warn!(
                            target: &self.address,
                            "Public key '{}' authentication failed: {}",
                            identity.comment(),
                            err
                        );
                        continue;
                    }
                }
            }

            Ok(session)
        }
    }

    async fn tcp_connect_timeout(&self) -> NetconfClientResult<TcpStream> {
        let stream = timeout(
            Duration::from_secs(10),
            TcpStream::connect(&(self.address.as_str(), self.port)),
        )
        .await
        .map_err(|e| NetconfClientError::new(e.to_string()))?;
        Ok(stream?)
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
