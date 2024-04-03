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
        let port: u16;
        let address: String;
        match addr.contains(':') {
            true => {
                let parts: Vec<&str> = addr.split(':').collect();
                address = parts[0].to_string();
                port = parts[1].parse().unwrap();
            }
            false => {
                address = addr.to_string();
                port = 830;
            }
        };

        let auth_user: String;

        if let Some(user) = username {
            auth_user = user.clone();
        } else if let Some(user) = params.user.as_deref() {
            auth_user = user.to_string();
        } else {
            return Err(NetconfClientError::new("No username provided".to_string()));
        }

        let auth_password: Option<String>;
        if password.is_some() {
            auth_password = password.clone();
        } else if params.identity_file.is_none() {
            return Err(NetconfClientError::new(
                "No password or identity file provided".to_string(),
            ));
        } else {
            auth_password = None;
        }

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
            && self.params.server_alive_interval.is_some()
        {
            let interval = self.params.server_alive_interval.unwrap().as_secs() as u32;
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
    if let Some(algos) = params.kex_algorithms.as_deref() {
        session
            .method_pref(MethodType::Kex, algos.join(",").as_str())
            .await?;
    }
    if let Some(algos) = params.host_key_algorithms.as_deref() {
        session
            .method_pref(MethodType::HostKey, algos.join(",").as_str())
            .await?;
    }
    if let Some(algos) = params.ciphers.as_deref() {
        session
            .method_pref(MethodType::CryptCs, algos.join(",").as_str())
            .await?;
    }
    if let Some(algos) = params.mac.as_deref() {
        session
            .method_pref(MethodType::MacCs, algos.join(",").as_str())
            .await?;
        session
            .method_pref(MethodType::MacSc, algos.join(",").as_str())
            .await?;
    }
    Ok(())
}
