//! SSH transport that requests the `netconf` subsystem ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).

use crate::error::{NetconfClientError, NetconfClientResult};
use crate::framer::Framer;
use crate::framer::async_framer::AsyncFramer;
use crate::transport::Transport;
use async_trait::async_trait;
use core::fmt;
use core::time::Duration;
use log::{debug, warn};
use russh::client::{self, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{
    self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts,
};
use russh::{ChannelStream, Disconnect, Preferred};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use zeroize::Zeroizing;

const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How to authenticate the SSH user.
///
/// Passwords and key passphrases are overwritten on drop. Prefer
/// [`SshAuth::password`] / [`SshAuth::key_file`].
#[derive(Clone, PartialEq, Eq)]
pub enum SshAuth {
    /// Password authentication.
    Password(Zeroizing<String>),
    /// Walk identities from the local ssh-agent until one is accepted.
    Agent,
    /// Public-key file. `passphrase` unlocks an encrypted key.
    KeyFile {
        /// Path to the private key.
        path: PathBuf,
        /// Passphrase for an encrypted key, if any.
        passphrase: Option<Zeroizing<String>>,
    },
}

impl SshAuth {
    /// Password authentication. Secret is overwritten on drop.
    pub fn password(password: impl Into<String>) -> Self {
        Self::Password(Zeroizing::new(password.into()))
    }

    /// Public-key file. `passphrase` is zeroized on drop when set.
    pub fn key_file(path: impl Into<PathBuf>, passphrase: Option<String>) -> Self {
        Self::KeyFile {
            path: path.into(),
            passphrase: passphrase.map(Zeroizing::new),
        }
    }
}

impl fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password(_) => f.write_str("Password(..)"),
            Self::Agent => f.write_str("Agent"),
            Self::KeyFile { path, passphrase } => f
                .debug_struct("KeyFile")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| ".."))
                .finish(),
        }
    }
}

/// What to do with the server's SSH host key.
///
/// Default on [`SshConfig`] / [`SshJump`] is [`HostKeyPolicy::RejectAll`]
/// (fail closed). Pin a fingerprint or an OpenSSH `known_hosts` file for
/// production; [`HostKeyPolicy::AcceptAll`] is an explicit lab opt-in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostKeyPolicy {
    /// Reject every host key.
    RejectAll,
    /// Accept only this SHA-256 fingerprint.
    ///
    /// The value may be written with or without a `SHA256:` prefix, and with
    /// or without base64 padding. Obtain it with
    /// `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`.
    Fingerprint(String),
    /// Look the host up in an OpenSSH `known_hosts` file.
    ///
    /// Missing file, missing host, and key mismatch all fail closed.
    KnownHosts(PathBuf),
    /// Trust on first use: accept a host that is not in `known_hosts` and
    /// append the key; reject a host whose key has changed.
    AcceptNew(PathBuf),
    /// Accept any host key (**insecure**). Logs a warning.
    AcceptAll,
}

/// SSH session knobs applied before the handshake.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshSessionOpts {
    compression: Option<bool>,
    keepalive: Option<Duration>,
    kex: Option<String>,
    host_key_algs: Option<String>,
    ciphers: Option<String>,
    macs: Option<String>,
}

impl SshSessionOpts {
    /// Empty opts: library defaults, no compression preference, no keepalive.
    pub fn new() -> Self {
        Self::default()
    }

    /// Negotiate zlib compression (`Compression` in `ssh_config(5)`).
    pub fn compression(mut self, enabled: bool) -> Self {
        self.compression = Some(enabled);
        self
    }

    /// SSH-level keepalive interval (`ServerAliveInterval` in `ssh_config(5)`).
    pub fn keepalive(mut self, interval: Duration) -> Self {
        self.keepalive = Some(interval);
        self
    }

    /// Preferred key-exchange algorithms, comma-separated.
    pub fn kex_algorithms(mut self, prefs: impl Into<String>) -> Self {
        self.kex = Some(prefs.into());
        self
    }

    /// Preferred host-key algorithms, comma-separated.
    pub fn host_key_algorithms(mut self, prefs: impl Into<String>) -> Self {
        self.host_key_algs = Some(prefs.into());
        self
    }

    /// Preferred ciphers, comma-separated (client→server).
    pub fn ciphers(mut self, prefs: impl Into<String>) -> Self {
        self.ciphers = Some(prefs.into());
        self
    }

    /// Preferred MAC algorithms, comma-separated (both directions).
    pub fn macs(mut self, prefs: impl Into<String>) -> Self {
        self.macs = Some(prefs.into());
        self
    }

    /// Whether compression was set.
    pub fn compression_enabled(&self) -> Option<bool> {
        self.compression
    }

    /// Keepalive interval, if set.
    pub fn keepalive_interval(&self) -> Option<Duration> {
        self.keepalive
    }

    /// Key-exchange preference string, if set.
    pub fn kex(&self) -> Option<&str> {
        self.kex.as_deref()
    }

    /// Host-key algorithm preference string, if set.
    pub fn host_key_algs(&self) -> Option<&str> {
        self.host_key_algs.as_deref()
    }

    /// Cipher preference string, if set.
    pub fn ciphers_pref(&self) -> Option<&str> {
        self.ciphers.as_deref()
    }

    /// MAC preference string, if set.
    pub fn macs_pref(&self) -> Option<&str> {
        self.macs.as_deref()
    }
}

/// One ProxyJump hop. Chain several with [`SshConfig::jump`]. Jump auth is
/// independent of the device; do not reuse the device password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshJump {
    host: String,
    port: u16,
    username: String,
    auth: SshAuth,
    host_key: HostKeyPolicy,
    session: SshSessionOpts,
}

impl SshJump {
    /// Jump host, SSH port (usually 22), and user.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        auth: SshAuth,
    ) -> Self {
        SshJump {
            host: host.into(),
            port,
            username: username.into(),
            auth,
            host_key: HostKeyPolicy::RejectAll,
            session: SshSessionOpts::new(),
        }
    }

    /// Host-key policy; [`HostKeyPolicy::RejectAll`] if unset.
    pub fn host_key(mut self, policy: HostKeyPolicy) -> Self {
        self.host_key = policy;
        self
    }

    /// Compression, keepalive, and algorithm prefs for the jump session.
    pub fn session(mut self, opts: SshSessionOpts) -> Self {
        self.session = opts;
        self
    }

    /// Jump hostname or address.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Jump SSH port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Jump username.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Jump authentication.
    pub fn auth(&self) -> &SshAuth {
        &self.auth
    }

    /// Jump host-key policy.
    pub fn host_key_policy(&self) -> &HostKeyPolicy {
        &self.host_key
    }

    /// Jump session knobs.
    pub fn session_opts(&self) -> &SshSessionOpts {
        &self.session
    }
}

/// SSH connection parameters for [`SSHTransport::connect`].
///
/// Host-key policy defaults to [`HostKeyPolicy::RejectAll`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfig {
    host: String,
    port: u16,
    username: String,
    auth: SshAuth,
    host_key: HostKeyPolicy,
    jumps: Vec<SshJump>,
    session: SshSessionOpts,
}

impl SshConfig {
    /// Device host, NETCONF port (usually [`crate::DEFAULT_NETCONF_SSH_PORT`]),
    /// and user.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        auth: SshAuth,
    ) -> Self {
        SshConfig {
            host: host.into(),
            port,
            username: username.into(),
            auth,
            host_key: HostKeyPolicy::RejectAll,
            jumps: Vec::new(),
            session: SshSessionOpts::new(),
        }
    }

    /// Host-key policy; [`HostKeyPolicy::RejectAll`] if unset.
    pub fn host_key(mut self, policy: HostKeyPolicy) -> Self {
        self.host_key = policy;
        self
    }

    /// Append a ProxyJump hop. Call once per hop, first hop first.
    pub fn jump(mut self, jump: SshJump) -> Self {
        self.jumps.push(jump);
        self
    }

    /// Compression, keepalive, and algorithm prefs for the device session.
    pub fn session(mut self, opts: SshSessionOpts) -> Self {
        self.session = opts;
        self
    }

    /// Device hostname or address.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Device NETCONF port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Device username.
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Device authentication.
    pub fn auth(&self) -> &SshAuth {
        &self.auth
    }

    /// Device host-key policy.
    pub fn host_key_policy(&self) -> &HostKeyPolicy {
        &self.host_key
    }

    /// ProxyJump hops, first hop first.
    pub fn jumps(&self) -> &[SshJump] {
        &self.jumps
    }

    /// Device session knobs.
    pub fn session_opts(&self) -> &SshSessionOpts {
        &self.session
    }
}

/// Authenticated ProxyJump chain that can open many device sessions.
///
/// Clone and share across tasks. Dropping the last clone closes the jump
/// SSH sessions. Device [`SSHTransport::close`] does not disconnect the jump.
///
/// Use [`JumpPool`] to share one chain across many devices.
#[derive(Clone)]
pub struct JumpSession {
    last: Arc<tokio::sync::Mutex<Handle<ClientHandler>>>,
    _upstream: Vec<Arc<tokio::sync::Mutex<Handle<ClientHandler>>>>,
    hops: Vec<(String, u16)>,
}

impl fmt::Debug for JumpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JumpSession")
            .field("hops", &self.hops)
            .finish()
    }
}

impl fmt::Display for JumpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_hops(
            self.hops.iter().map(|(host, port)| (host.as_str(), *port)),
        ))
    }
}

impl JumpSession {
    /// Handshake the full jump chain. `jumps` must not be empty.
    pub async fn connect(jumps: &[SshJump]) -> NetconfClientResult<Self> {
        let mut hops = jumps.iter();
        let first = hops.next().ok_or_else(|| {
            NetconfClientError::new("JumpSession::connect requires at least one hop")
        })?;
        debug!("Connecting via ProxyJump {}:{}", first.host, first.port);
        let mut last = handshake_and_auth(
            connect_tcp(&first.host, first.port).await?,
            target_from_jump(first),
        )
        .await?;
        let mut upstream = Vec::new();
        for next in hops {
            debug!("ProxyJump next hop {}:{}", next.host, next.port);
            let stream = open_jump(&last, &next.host, next.port).await?;
            upstream.push(Arc::new(tokio::sync::Mutex::new(last)));
            last = handshake_and_auth(stream, target_from_jump(next)).await?;
        }
        Ok(Self {
            last: Arc::new(tokio::sync::Mutex::new(last)),
            _upstream: upstream,
            hops: jumps
                .iter()
                .map(|jump| (jump.host.clone(), jump.port))
                .collect(),
        })
    }

    /// Open a NETCONF session to `config.host` through this jump chain.
    ///
    /// `config.jumps` are ignored; the path is this session.
    pub async fn connect_device(&self, config: SshConfig) -> NetconfClientResult<SSHTransport> {
        let stream = {
            let handle = self.last.lock().await;
            if handle.is_closed() {
                return Err(NetconfClientError::new(format!(
                    "jump session {self} is closed"
                )));
            }
            open_jump(&handle, &config.host, config.port).await?
        };
        let session = handshake_and_auth(stream, target_from_config(&config)).await?;
        let mut transport = open_netconf(session).await?;
        transport._jumps = Some(self.clone());
        Ok(transport)
    }

    /// True when the last hop can no longer open channels.
    pub fn is_closed(&self) -> bool {
        self.last
            .try_lock()
            .map(|handle| handle.is_closed())
            .unwrap_or(false)
    }
}

/// One shared [`JumpSession`] for a ProxyJump chain.
///
/// Handshake is lazy and serialized. Device channels open in parallel;
/// cap concurrency at the caller (`--parallel` in the CLI).
pub struct JumpPool {
    hops: Vec<SshJump>,
    session: tokio::sync::Mutex<Option<JumpSession>>,
}

impl fmt::Debug for JumpPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JumpPool")
            .field("hops", &format_jump_hops(&self.hops))
            .finish()
    }
}

impl JumpPool {
    /// Pool for this hop chain. Does not connect until the first device.
    pub fn new(jumps: impl Into<Vec<SshJump>>) -> NetconfClientResult<Self> {
        let hops = jumps.into();
        if hops.is_empty() {
            return Err(NetconfClientError::new(
                "JumpPool::new requires at least one hop",
            ));
        }
        Ok(Self {
            hops,
            session: tokio::sync::Mutex::new(None),
        })
    }

    /// ProxyJump hops this pool authenticates, first hop first.
    pub fn jumps(&self) -> &[SshJump] {
        &self.hops
    }

    /// Open a NETCONF session to `config.host` through the shared jump session.
    ///
    /// `config.jumps` are ignored; the path is this pool.
    pub async fn connect_device(&self, config: SshConfig) -> NetconfClientResult<SSHTransport> {
        self.session().await?.connect_device(config).await
    }

    async fn session(&self) -> NetconfClientResult<JumpSession> {
        let mut slot = self.session.lock().await;
        if let Some(session) = slot.as_ref()
            && !session.is_closed()
        {
            return Ok(session.clone());
        }
        // Hold the lock so a fan-out shares the first handshake.
        let session = JumpSession::connect(&self.hops).await?;
        *slot = Some(session.clone());
        Ok(session)
    }
}

/// NETCONF-over-SSH session on a russh channel.
///
/// Default NETCONF port is **830**; pass it in [`SshConfig::new`].
pub struct SSHTransport {
    session: Handle<ClientHandler>,
    framer: AsyncFramer<ChannelStream<client::Msg>>,
    /// Keeps the ProxyJump chain alive for the life of the device channel.
    _jumps: Option<JumpSession>,
}

impl SSHTransport {
    /// TCP connect, authenticate, verify the host key, request subsystem `netconf`.
    ///
    /// Optional ProxyJump chain. Host-key policy defaults to
    /// [`HostKeyPolicy::RejectAll`]. Share a [`JumpPool`] when many devices
    /// use the same hops.
    pub async fn connect(config: SshConfig) -> NetconfClientResult<SSHTransport> {
        if config.jumps.is_empty() {
            let stream = connect_tcp(&config.host, config.port).await?;
            let session = handshake_and_auth(stream, target_from_config(&config)).await?;
            return open_netconf(session).await;
        }
        JumpSession::connect(&config.jumps)
            .await?
            .connect_device(config)
            .await
    }
}

#[async_trait]
impl Transport for SSHTransport {
    async fn receive(&mut self) -> NetconfClientResult<String> {
        self.framer.read_async().await
    }

    async fn write(&mut self, rpc: &str) -> NetconfClientResult<()> {
        self.framer.write_async(rpc).await
    }
    async fn write_and_receive(&mut self, rpc: &str) -> NetconfClientResult<String> {
        self.framer.write_async(rpc).await?;
        self.framer.read_async().await
    }

    async fn close(&mut self) -> NetconfClientResult<()> {
        if let Err(err) = self.framer.channel_mut().shutdown().await {
            debug!("SSH channel shutdown: {err}");
        }
        self.session
            .disconnect(Disconnect::ByApplication, "Shutdown", "")
            .await
            .map_err(map_russh)?;
        self._jumps = None;
        Ok(())
    }

    async fn upgrade(&mut self) {
        self.framer.upgrade().await;
    }
}

struct Target<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    auth: &'a SshAuth,
    host_key: &'a HostKeyPolicy,
    session: &'a SshSessionOpts,
}

fn target_from_config(config: &SshConfig) -> Target<'_> {
    Target {
        host: &config.host,
        port: config.port,
        username: &config.username,
        auth: &config.auth,
        host_key: &config.host_key,
        session: &config.session,
    }
}

fn target_from_jump(jump: &SshJump) -> Target<'_> {
    Target {
        host: &jump.host,
        port: jump.port,
        username: &jump.username,
        auth: &jump.auth,
        host_key: &jump.host_key,
        session: &jump.session,
    }
}

struct ClientHandler {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
}

#[derive(Debug)]
enum SshConnectError {
    Client(NetconfClientError),
    Protocol(russh::Error),
}

impl From<russh::Error> for SshConnectError {
    fn from(err: russh::Error) -> Self {
        Self::Protocol(err)
    }
}

impl From<NetconfClientError> for SshConnectError {
    fn from(err: NetconfClientError) -> Self {
        Self::Client(err)
    }
}

impl client::Handler for ClientHandler {
    type Error = SshConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        verify_host_key(
            &self.host,
            self.port,
            &self.policy,
            &server_public_key.public_key(),
        )?;
        Ok(true)
    }
}

async fn connect_tcp(host: &str, port: u16) -> NetconfClientResult<TcpStream> {
    timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| NetconfClientError::new(format!("timeout connecting to {host}:{port}")))?
        .map_err(Into::into)
}

async fn open_jump(
    jump_session: &Handle<ClientHandler>,
    host: &str,
    port: u16,
) -> NetconfClientResult<ChannelStream<client::Msg>> {
    let channel = jump_session
        .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 22)
        .await
        .map_err(map_russh)?;
    Ok(channel.into_stream())
}

async fn handshake_and_auth<R>(
    stream: R,
    target: Target<'_>,
) -> NetconfClientResult<Handle<ClientHandler>>
where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ssh_config = build_client_config(target.session)?;
    let handler = ClientHandler {
        host: target.host.to_string(),
        port: target.port,
        policy: target.host_key.clone(),
    };
    let mut session = client::connect_stream(Arc::new(ssh_config), stream, handler)
        .await
        .map_err(map_connect)?;
    authenticate(
        &mut session,
        target.host,
        target.port,
        target.username,
        target.auth,
    )
    .await?;
    Ok(session)
}

async fn open_netconf(session: Handle<ClientHandler>) -> NetconfClientResult<SSHTransport> {
    let channel = session.channel_open_session().await.map_err(map_russh)?;
    channel
        .request_subsystem(true, "netconf")
        .await
        .map_err(map_russh)?;
    Ok(SSHTransport {
        session,
        framer: AsyncFramer::new(channel.into_stream()),
        _jumps: None,
    })
}

fn build_client_config(opts: &SshSessionOpts) -> NetconfClientResult<client::Config> {
    let mut preferred = Preferred::DEFAULT;
    if let Some(enabled) = opts.compression {
        preferred.compression = if enabled {
            Cow::Owned(vec![
                russh::compression::ZLIB,
                russh::compression::ZLIB_LEGACY,
                russh::compression::NONE,
            ])
        } else {
            Cow::Owned(vec![russh::compression::NONE])
        };
    }
    if let Some(kex) = opts.kex.as_deref() {
        preferred.kex = Cow::Owned(parse_kex(kex)?);
    }
    if let Some(host_key) = opts.host_key_algs.as_deref() {
        preferred.key = Cow::Owned(parse_host_keys(host_key)?);
    }
    if let Some(ciphers) = opts.ciphers.as_deref() {
        preferred.cipher = Cow::Owned(parse_ciphers(ciphers)?);
    }
    if let Some(macs) = opts.macs.as_deref() {
        preferred.mac = Cow::Owned(parse_macs(macs)?);
    }
    Ok(client::Config {
        preferred,
        keepalive_interval: opts.keepalive,
        inactivity_timeout: None,
        nodelay: true,
        ..client::Config::default()
    })
}

fn split_prefs(prefs: &str) -> impl Iterator<Item = &str> {
    prefs
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn parse_kex(prefs: &str) -> NetconfClientResult<Vec<russh::kex::Name>> {
    let mut out = Vec::new();
    for name in split_prefs(prefs) {
        match russh::kex::Name::try_from(name) {
            Ok(alg) => {
                if !out.contains(&alg) {
                    out.push(alg);
                }
            }
            Err(()) => warn!("ignoring unknown SSH kex algorithm {name}"),
        }
    }
    if out.is_empty() {
        return Err(NetconfClientError::new(format!(
            "no supported SSH kex algorithms in '{prefs}'"
        )));
    }
    for ext in [
        russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
        russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    ] {
        if !out.contains(&ext) {
            out.push(ext);
        }
    }
    Ok(out)
}

fn parse_host_keys(prefs: &str) -> NetconfClientResult<Vec<keys::Algorithm>> {
    let mut out = Vec::new();
    for name in split_prefs(prefs) {
        match keys::Algorithm::new(name) {
            Ok(alg) => {
                if !out.contains(&alg) {
                    out.push(alg);
                }
            }
            Err(err) => warn!("ignoring unknown SSH host-key algorithm {name}: {err}"),
        }
    }
    if out.is_empty() {
        return Err(NetconfClientError::new(format!(
            "no supported SSH host-key algorithms in '{prefs}'"
        )));
    }
    Ok(out)
}

fn parse_ciphers(prefs: &str) -> NetconfClientResult<Vec<russh::cipher::Name>> {
    let mut out = Vec::new();
    for name in split_prefs(prefs) {
        match russh::cipher::Name::try_from(name) {
            Ok(alg) => {
                if !out.contains(&alg) {
                    out.push(alg);
                }
            }
            Err(()) => warn!("ignoring unknown SSH cipher {name}"),
        }
    }
    if out.is_empty() {
        return Err(NetconfClientError::new(format!(
            "no supported SSH ciphers in '{prefs}'"
        )));
    }
    Ok(out)
}

fn parse_macs(prefs: &str) -> NetconfClientResult<Vec<russh::mac::Name>> {
    let mut out = Vec::new();
    for name in split_prefs(prefs) {
        match russh::mac::Name::try_from(name) {
            Ok(alg) => {
                if !out.contains(&alg) {
                    out.push(alg);
                }
            }
            Err(()) => warn!("ignoring unknown SSH MAC algorithm {name}"),
        }
    }
    if out.is_empty() {
        return Err(NetconfClientError::new(format!(
            "no supported SSH MAC algorithms in '{prefs}'"
        )));
    }
    Ok(out)
}

fn verify_host_key(
    host: &str,
    port: u16,
    policy: &HostKeyPolicy,
    key: &PublicKey,
) -> NetconfClientResult<()> {
    match policy {
        HostKeyPolicy::KnownHosts(path) => {
            return check_known_hosts(host, port, path, key, false);
        }
        HostKeyPolicy::AcceptNew(path) => {
            return check_known_hosts(host, port, path, key, true);
        }
        _ => {}
    }
    let fingerprint = host_key_fingerprint(key);
    if evaluate_host_key_policy(policy, &fingerprint) {
        if matches!(policy, HostKeyPolicy::AcceptAll) {
            warn!(
                "accepting SSH host key for {host} without verification ({fingerprint}) — \
                 set HostKeyPolicy::Fingerprint or HostKeyPolicy::KnownHosts for production use"
            );
        }
        return Ok(());
    }
    let reason = match policy {
        HostKeyPolicy::RejectAll => "RejectAll policy; pin the fingerprint or a known_hosts file",
        HostKeyPolicy::Fingerprint(_) => "fingerprint does not match",
        HostKeyPolicy::KnownHosts(_) | HostKeyPolicy::AcceptNew(_) => {
            unreachable!("file policies return earlier")
        }
        HostKeyPolicy::AcceptAll => unreachable!("AcceptAll accepts every key"),
    };
    Err(NetconfClientError::HostKeyRejected {
        host: host.to_string(),
        fingerprint,
        reason: reason.to_string(),
    })
}

fn check_known_hosts(
    host: &str,
    port: u16,
    path: &Path,
    key: &PublicKey,
    accept_new: bool,
) -> NetconfClientResult<()> {
    let fingerprint = host_key_fingerprint(key);
    if !path.exists() && !accept_new {
        return Err(NetconfClientError::new(format!(
            "failed to read known_hosts {}: file not found",
            path.display()
        )));
    }
    match known_hosts_action(
        keys::check_known_hosts_path(host, port, key, path),
        accept_new,
    ) {
        KnownHostsAction::Allow => Ok(()),
        KnownHostsAction::Pin => pin_new_host(path, host, port, key, &fingerprint),
        KnownHostsAction::Reject(reason) => Err(NetconfClientError::HostKeyRejected {
            host: host.to_string(),
            fingerprint,
            reason: reason.to_string(),
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum KnownHostsAction {
    Allow,
    Pin,
    Reject(&'static str),
}

#[derive(Debug)]
enum KnownHostsCheck {
    Match,
    NotFound,
    Mismatch,
    Failure,
}

fn known_hosts_action(result: Result<bool, keys::Error>, accept_new: bool) -> KnownHostsAction {
    known_hosts_action_from(classify_known_hosts(result), accept_new)
}

fn classify_known_hosts(result: Result<bool, keys::Error>) -> KnownHostsCheck {
    match result {
        Ok(true) => KnownHostsCheck::Match,
        Ok(false) => KnownHostsCheck::NotFound,
        Err(keys::Error::KeyChanged { .. }) => KnownHostsCheck::Mismatch,
        Err(_) => KnownHostsCheck::Failure,
    }
}

fn known_hosts_action_from(check: KnownHostsCheck, accept_new: bool) -> KnownHostsAction {
    match check {
        KnownHostsCheck::Match => KnownHostsAction::Allow,
        KnownHostsCheck::NotFound if accept_new => KnownHostsAction::Pin,
        other => KnownHostsAction::Reject(known_hosts_reason(other)),
    }
}

fn pin_new_host(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
    fingerprint: &str,
) -> NetconfClientResult<()> {
    known_hosts::learn_known_hosts_path(host, port, key, path).map_err(|err| {
        NetconfClientError::new(format!(
            "failed to record host key for {host} in {}: {err}",
            path.display()
        ))
    })?;
    warn!(
        "accepted new SSH host key for {host} ({fingerprint}); stored in {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
fn known_hosts_host_spec(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn known_hosts_reason(check: KnownHostsCheck) -> &'static str {
    match check {
        KnownHostsCheck::Match => "matched",
        KnownHostsCheck::Mismatch => "key does not match known_hosts",
        KnownHostsCheck::NotFound => "host not in known_hosts",
        KnownHostsCheck::Failure => "known_hosts check failed",
    }
}

fn host_key_fingerprint(key: &PublicKey) -> String {
    format!("{}", key.fingerprint(HashAlg::Sha256))
}

/// Decide whether `actual_fingerprint` (`SHA256:…`) is accepted under `policy`.
///
/// Pure so the decision is unit-testable without a live SSH handshake.
fn evaluate_host_key_policy(policy: &HostKeyPolicy, actual_fingerprint: &str) -> bool {
    match policy {
        HostKeyPolicy::AcceptAll => true,
        HostKeyPolicy::RejectAll => false,
        HostKeyPolicy::Fingerprint(expected) => fingerprints_match(expected, actual_fingerprint),
        HostKeyPolicy::KnownHosts(_) | HostKeyPolicy::AcceptNew(_) => false,
    }
}

fn fingerprints_match(expected: &str, actual: &str) -> bool {
    let expected = normalize_fingerprint(expected);
    let actual = normalize_fingerprint(actual);
    !expected.is_empty() && expected == actual
}

fn normalize_fingerprint(value: &str) -> String {
    value
        .trim()
        .strip_prefix("SHA256:")
        .unwrap_or(value.trim())
        .trim_end_matches('=')
        .to_string()
}

async fn authenticate(
    session: &mut Handle<ClientHandler>,
    host: &str,
    port: u16,
    username: &str,
    auth: &SshAuth,
) -> NetconfClientResult<()> {
    let success = match auth {
        SshAuth::Password(password) => session
            .authenticate_password(username, password.as_str())
            .await
            .map_err(map_russh)?
            .success(),
        SshAuth::Agent => authenticate_agent(session, username).await?,
        SshAuth::KeyFile { path, passphrase } => {
            authenticate_key_file(
                session,
                username,
                path,
                passphrase.as_ref().map(|secret| secret.as_str()),
            )
            .await?
        }
    };
    if success {
        Ok(())
    } else {
        Err(NetconfClientError::new(format!(
            "SSH authentication failed for {username}@{host}:{port}"
        )))
    }
}

async fn authenticate_key_file(
    session: &mut Handle<ClientHandler>,
    username: &str,
    path: &Path,
    passphrase: Option<&str>,
) -> NetconfClientResult<bool> {
    let key = keys::load_secret_key(path, passphrase).map_err(map_keys)?;
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(map_russh)?
        .flatten();
    Ok(session
        .authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(map_russh)?
        .success())
}

async fn authenticate_agent(
    session: &mut Handle<ClientHandler>,
    username: &str,
) -> NetconfClientResult<bool> {
    let mut agent = connect_agent().await?;
    let identities = agent.request_identities().await.map_err(map_keys)?;
    if identities.is_empty() {
        return Ok(false);
    }
    let hash = session
        .best_supported_rsa_hash()
        .await
        .map_err(map_russh)?
        .flatten();
    for identity in identities {
        let result = match identity {
            AgentIdentity::PublicKey { key, comment } => {
                debug!("trying ssh-agent identity {comment}");
                session
                    .authenticate_publickey_with(username, key, hash, &mut agent)
                    .await
            }
            AgentIdentity::Certificate {
                certificate,
                comment,
            } => {
                debug!("trying ssh-agent certificate {comment}");
                session
                    .authenticate_certificate_with(username, certificate, hash, &mut agent)
                    .await
            }
        };
        match result {
            Ok(auth) if auth.success() => return Ok(true),
            Ok(_) => continue,
            Err(err) => {
                debug!("ssh-agent identity rejected: {err}");
            }
        }
    }
    Ok(false)
}

type DynAgent = AgentClient<Box<dyn keys::agent::client::AgentStream + Send + Unpin>>;

async fn connect_agent() -> NetconfClientResult<DynAgent> {
    #[cfg(unix)]
    {
        AgentClient::connect_env()
            .await
            .map(AgentClient::dynamic)
            .map_err(map_keys)
    }
    #[cfg(windows)]
    {
        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK")
            && let Ok(client) = AgentClient::connect_named_pipe(&sock).await
        {
            return Ok(client.dynamic());
        }
        if let Ok(client) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            return Ok(client.dynamic());
        }
        AgentClient::connect_pageant()
            .await
            .map(AgentClient::dynamic)
            .map_err(map_keys)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(NetconfClientError::new(
            "ssh-agent is not supported on this platform",
        ))
    }
}

fn format_hops<'a>(hops: impl Iterator<Item = (&'a str, u16)>) -> String {
    hops.map(|(host, port)| format!("{host}:{port}"))
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn format_jump_hops(jumps: &[SshJump]) -> String {
    format_hops(jumps.iter().map(|jump| (jump.host.as_str(), jump.port)))
}

fn map_connect(err: SshConnectError) -> NetconfClientError {
    match err {
        SshConnectError::Client(err) => err,
        SshConnectError::Protocol(err) => map_russh(err),
    }
}

fn map_russh(err: russh::Error) -> NetconfClientError {
    match err {
        russh::Error::IO(io) => NetconfClientError::Io(io),
        russh::Error::HUP | russh::Error::Disconnect | russh::Error::RecvError => {
            NetconfClientError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                err.to_string(),
            ))
        }
        other => NetconfClientError::Ssh(other.to_string()),
    }
}

fn map_keys(err: keys::Error) -> NetconfClientError {
    match err {
        keys::Error::IO(io) => NetconfClientError::Io(io),
        other => NetconfClientError::Ssh(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_defaults_to_reject_all_and_no_jump() {
        let cfg = SshConfig::new("192.0.2.10", 830, "netconf", SshAuth::password("x"));
        assert_eq!(cfg.host(), "192.0.2.10");
        assert_eq!(cfg.port(), 830);
        assert_eq!(cfg.username(), "netconf");
        assert_eq!(cfg.host_key_policy(), &HostKeyPolicy::RejectAll);
        assert!(cfg.jumps().is_empty());
        assert!(matches!(cfg.auth(), SshAuth::Password(_)));
    }

    #[test]
    fn ssh_config_builder_sets_fingerprint_and_jump() {
        let jump = SshJump::new("jump", 22, "jump-user", SshAuth::Agent)
            .host_key(HostKeyPolicy::AcceptAll)
            .session(SshSessionOpts::new().compression(true));
        let cfg = SshConfig::new("192.0.2.10", 830, "netconf", SshAuth::Agent)
            .host_key(HostKeyPolicy::Fingerprint("SHA256:abc".into()))
            .session(
                SshSessionOpts::new()
                    .kex_algorithms("curve25519-sha256")
                    .keepalive(Duration::from_secs(15)),
            )
            .jump(jump.clone());
        assert_eq!(
            cfg.host_key_policy(),
            &HostKeyPolicy::Fingerprint("SHA256:abc".into())
        );
        assert_eq!(cfg.session_opts().kex(), Some("curve25519-sha256"));
        assert_eq!(
            cfg.session_opts().keepalive_interval(),
            Some(Duration::from_secs(15))
        );
        let hop = &cfg.jumps()[0];
        assert_eq!(hop.host(), "jump");
        assert_eq!(hop.port(), 22);
        assert_eq!(hop.username(), "jump-user");
        assert_eq!(hop.auth(), &SshAuth::Agent);
        assert_eq!(hop.host_key_policy(), &HostKeyPolicy::AcceptAll);
        assert_eq!(hop.session_opts().compression_enabled(), Some(true));
    }

    #[test]
    fn ssh_config_jump_appends_hops_in_order() {
        let cfg = SshConfig::new("192.0.2.10", 830, "netconf", SshAuth::Agent)
            .jump(SshJump::new("jump1", 22, "u1", SshAuth::Agent))
            .jump(SshJump::new("jump2", 2222, "u2", SshAuth::Agent));
        let hops = cfg.jumps();
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].host(), "jump1");
        assert_eq!(hops[0].port(), 22);
        assert_eq!(hops[1].host(), "jump2");
        assert_eq!(hops[1].port(), 2222);
    }

    #[test]
    fn known_hosts_reason_maps_check_results() {
        assert_eq!(
            known_hosts_reason(KnownHostsCheck::Mismatch),
            "key does not match known_hosts"
        );
        assert_eq!(
            known_hosts_reason(KnownHostsCheck::NotFound),
            "host not in known_hosts"
        );
        assert_eq!(
            known_hosts_reason(KnownHostsCheck::Failure),
            "known_hosts check failed"
        );
    }

    #[test]
    fn accept_new_pins_unknown_and_rejects_mismatch() {
        assert_eq!(
            known_hosts_action_from(KnownHostsCheck::NotFound, true),
            KnownHostsAction::Pin
        );
        assert_eq!(
            known_hosts_action_from(KnownHostsCheck::Mismatch, true),
            KnownHostsAction::Reject("key does not match known_hosts")
        );
        assert_eq!(
            known_hosts_action_from(KnownHostsCheck::NotFound, false),
            KnownHostsAction::Reject("host not in known_hosts")
        );
        assert_eq!(
            known_hosts_action_from(KnownHostsCheck::Match, true),
            KnownHostsAction::Allow
        );
    }

    #[test]
    fn classify_known_hosts_maps_russh_results() {
        assert!(matches!(
            classify_known_hosts(Ok(true)),
            KnownHostsCheck::Match
        ));
        assert!(matches!(
            classify_known_hosts(Ok(false)),
            KnownHostsCheck::NotFound
        ));
        assert!(matches!(
            classify_known_hosts(Err(keys::Error::KeyChanged { line: 3 })),
            KnownHostsCheck::Mismatch
        ));
        assert!(matches!(
            classify_known_hosts(Err(keys::Error::CouldNotReadKey)),
            KnownHostsCheck::Failure
        ));
    }

    #[test]
    fn known_hosts_host_spec_uses_brackets_when_not_port_22() {
        assert_eq!(known_hosts_host_spec("router", 22), "router");
        assert_eq!(known_hosts_host_spec("router", 830), "[router]:830");
        assert_eq!(
            known_hosts_host_spec("2001:db8::1", 830),
            "[2001:db8::1]:830"
        );
    }

    #[test]
    fn ssh_err_wraps_without_leaking_the_crate_type() {
        let io = map_russh(russh::Error::IO(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "pipe closed",
        )));
        assert!(
            matches!(&io, NetconfClientError::Io(err) if err.kind() == std::io::ErrorKind::UnexpectedEof),
            "{io:?}"
        );
        let eof = map_russh(russh::Error::HUP);
        assert!(
            matches!(&eof, NetconfClientError::Io(err) if err.kind() == std::io::ErrorKind::UnexpectedEof),
            "{eof:?}"
        );
        let other = map_russh(russh::Error::NotAuthenticated);
        assert!(
            matches!(&other, NetconfClientError::Ssh(msg) if msg.to_ascii_lowercase().contains("authenticated")),
            "{other:?}"
        );
    }

    #[test]
    fn reject_all_rejects_and_accept_all_accepts() {
        assert!(!evaluate_host_key_policy(
            &HostKeyPolicy::RejectAll,
            "SHA256:abc"
        ));
        assert!(evaluate_host_key_policy(
            &HostKeyPolicy::AcceptAll,
            "SHA256:abc"
        ));
    }

    #[test]
    fn fingerprint_match_is_tolerant_of_prefix_and_padding() {
        let actual = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
        assert!(evaluate_host_key_policy(
            &HostKeyPolicy::Fingerprint(actual.into()),
            actual
        ));
        assert!(evaluate_host_key_policy(
            &HostKeyPolicy::Fingerprint("47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU".into()),
            actual
        ));
        assert!(evaluate_host_key_policy(
            &HostKeyPolicy::Fingerprint(
                "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=".into()
            ),
            actual
        ));
        assert!(!evaluate_host_key_policy(
            &HostKeyPolicy::Fingerprint("SHA256:nope".into()),
            actual
        ));
        assert!(!evaluate_host_key_policy(
            &HostKeyPolicy::Fingerprint(String::new()),
            actual
        ));
    }

    #[test]
    fn ssh_auth_debug_redacts_secrets() {
        let password = format!("{:?}", SshAuth::password("secret"));
        assert!(!password.contains("secret"), "{password}");
        let key = format!("{:?}", SshAuth::key_file("/tmp/id", Some("s3cret".into())));
        assert!(!key.contains("s3cret"), "{key}");
        assert!(key.contains("/tmp/id"), "{key}");
    }

    #[test]
    fn parse_kex_keeps_known_and_appends_extensions() {
        let parsed = parse_kex("curve25519-sha256,not-a-real-kex").unwrap();
        assert!(parsed.contains(&russh::kex::CURVE25519));
        assert!(parsed.contains(&russh::kex::EXTENSION_SUPPORT_AS_CLIENT));
        assert!(parsed.contains(&russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT));
        assert_eq!(parsed[0], russh::kex::CURVE25519);
    }

    #[test]
    fn parse_kex_rejects_empty_or_unknown_only() {
        assert!(parse_kex("").is_err());
        assert!(parse_kex("not-a-real-kex").is_err());
    }

    #[test]
    fn parse_ciphers_and_macs_keep_known() {
        let ciphers = parse_ciphers("aes256-ctr,chacha20-poly1305@openssh.com").unwrap();
        assert_eq!(ciphers[0], russh::cipher::AES_256_CTR);
        assert_eq!(ciphers[1], russh::cipher::CHACHA20_POLY1305);
        let macs = parse_macs("hmac-sha2-256,hmac-sha2-512").unwrap();
        assert_eq!(macs[0], russh::mac::HMAC_SHA256);
        assert_eq!(macs[1], russh::mac::HMAC_SHA512);
    }

    #[test]
    fn parse_host_keys_keeps_known() {
        let keys = parse_host_keys("ssh-ed25519,rsa-sha2-256").unwrap();
        assert_eq!(keys[0], russh::keys::Algorithm::Ed25519);
        assert_eq!(
            keys[1],
            russh::keys::Algorithm::Rsa {
                hash: Some(HashAlg::Sha256)
            }
        );
    }

    #[test]
    fn build_client_config_maps_compression_and_keepalive() {
        let opts = SshSessionOpts::new()
            .compression(true)
            .keepalive(Duration::from_secs(15))
            .ciphers("aes256-ctr");
        let cfg = build_client_config(&opts).unwrap();
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(15)));
        assert_eq!(cfg.preferred.cipher[0], russh::cipher::AES_256_CTR);
        assert_eq!(cfg.preferred.compression[0], russh::compression::ZLIB);
    }

    #[test]
    fn jump_pool_requires_a_hop() {
        let err = JumpPool::new(Vec::<SshJump>::new()).unwrap_err();
        assert!(err.to_string().contains("at least one hop"), "{err}");
    }

    #[tokio::test]
    async fn jump_session_requires_a_hop() {
        let err = JumpSession::connect(&[]).await.unwrap_err();
        assert!(err.to_string().contains("at least one hop"), "{err}");
    }

    #[test]
    fn format_jump_hops_joins_chain() {
        let hops = [
            SshJump::new("jump1", 22, "u1", SshAuth::Agent),
            SshJump::new("jump2", 2222, "u2", SshAuth::Agent),
        ];
        assert_eq!(format_jump_hops(&hops), "jump1:22 -> jump2:2222");
    }
}
