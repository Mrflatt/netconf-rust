//! SSH transport that requests the `netconf` subsystem ([RFC6242](https://www.rfc-editor.org/rfc/rfc6242.html)).

use crate::error::{NetconfClientError, NetconfClientResult};
use crate::framer::Framer;
use crate::framer::async_framer::AsyncFramer;
use crate::transport::Transport;
use async_ssh2_lite::{AsyncChannel, AsyncSession, SessionConfiguration, ssh2};
use async_trait::async_trait;
use core::fmt;
use core::time::Duration;
use log::{debug, warn};
use std::path::{Path, PathBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use zeroize::Zeroizing;

/// libssh2 caps every blocking call with this, reads included, so it has to
/// leave room for a slow device to answer a large `<get-config>`.
const SSH_TIMEOUT_MS: u32 = 30_000;
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
    /// Look the host up in an OpenSSH `known_hosts` file via libssh2.
    ///
    /// Missing file, missing host, and key mismatch all fail closed.
    KnownHosts(PathBuf),
    /// Trust on first use: accept a host that is not in `known_hosts` and
    /// append the key; reject a host whose key has changed.
    AcceptNew(PathBuf),
    /// Accept any host key (**insecure**). Logs a warning.
    AcceptAll,
}

/// libssh2 session knobs applied before the handshake.
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
    /// Empty opts: libssh2 defaults, no compression, no keepalive.
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

/// NETCONF-over-SSH session on a Tokio [`TcpStream`].
///
/// Default NETCONF port is **830**; pass it in [`SshConfig::new`].
pub struct SSHTransport {
    session: AsyncSession<TcpStream>,
    framer: AsyncFramer<AsyncChannel<TcpStream>>,
    /// Keeps each ProxyJump copy task (and thus each jump session) alive.
    _jumps: Vec<JoinHandle<()>>,
}

impl SSHTransport {
    /// TCP connect, authenticate, verify the host key, request subsystem `netconf`.
    ///
    /// Optional ProxyJump chain. Host-key policy defaults to
    /// [`HostKeyPolicy::RejectAll`].
    pub async fn connect(config: SshConfig) -> NetconfClientResult<SSHTransport> {
        let (stream, jumps) = if config.jumps.is_empty() {
            (connect_tcp(&config.host, config.port).await?, Vec::new())
        } else {
            connect_via_jumps(&config.host, config.port, &config.jumps).await?
        };
        match handshake_and_auth(stream, target_from_config(&config)).await {
            Ok(session) => {
                let mut transport = connect_internal(session).await?;
                transport._jumps = jumps;
                Ok(transport)
            }
            Err(err) => {
                for handle in jumps {
                    handle.abort();
                }
                Err(err)
            }
        }
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
        let channel = self.framer.channel_mut();
        channel.send_eof().await.ssh()?;
        channel.wait_eof().await.ssh()?;
        channel.close().await.ssh()?;
        channel.wait_close().await.ssh()?;
        self.session
            .disconnect(Some(ssh2::ByApplication), "Shutdown", None)
            .await
            .ssh()?;
        for handle in self._jumps.drain(..) {
            handle.abort();
        }
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

async fn connect_internal(session: AsyncSession<TcpStream>) -> NetconfClientResult<SSHTransport> {
    if session.authenticated() {
        let mut channel = session.channel_session().await.ssh()?;
        channel.subsystem("netconf").await.ssh()?;
        Ok(SSHTransport {
            session,
            framer: AsyncFramer::new(channel),
            _jumps: Vec::new(),
        })
    } else {
        Err(NetconfClientError::new(
            "SSH session is not authenticated; authenticate before requesting the netconf subsystem",
        ))
    }
}

async fn connect_tcp(host: &str, port: u16) -> NetconfClientResult<TcpStream> {
    timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| NetconfClientError::new(format!("timeout connecting to {host}:{port}")))?
        .map_err(Into::into)
}

async fn connect_via_jumps(
    host: &str,
    port: u16,
    jumps: &[SshJump],
) -> NetconfClientResult<(TcpStream, Vec<JoinHandle<()>>)> {
    let mut hops = jumps.iter();
    let first = hops
        .next()
        .expect("connect_via_jumps requires at least one hop");
    debug!("Connecting via ProxyJump {}:{}", first.host, first.port);
    let mut session = handshake_and_auth(
        connect_tcp(&first.host, first.port).await?,
        target_from_jump(first),
    )
    .await?;

    let mut handles = Vec::new();
    for next in hops {
        debug!("ProxyJump next hop {}:{}", next.host, next.port);
        match open_tunnel(session, &next.host, next.port).await {
            Ok((stream, handle)) => {
                handles.push(handle);
                session = match handshake_and_auth(stream, target_from_jump(next)).await {
                    Ok(session) => session,
                    Err(err) => {
                        abort_jumps(handles);
                        return Err(err);
                    }
                };
            }
            Err(err) => {
                abort_jumps(handles);
                return Err(err);
            }
        }
    }

    match open_tunnel(session, host, port).await {
        Ok((stream, handle)) => {
            handles.push(handle);
            Ok((stream, handles))
        }
        Err(err) => {
            abort_jumps(handles);
            Err(err)
        }
    }
}

fn abort_jumps(handles: Vec<JoinHandle<()>>) {
    for handle in handles {
        handle.abort();
    }
}

/// Expose a `direct-tcpip` channel as a local loopback socket.
///
/// `AsyncSession` needs a raw fd, so the tunneled channel is not wrapped
/// as a stream. Bind stays on 127.0.0.1.
async fn open_tunnel(
    jump_session: AsyncSession<TcpStream>,
    host: &str,
    port: u16,
) -> NetconfClientResult<(TcpStream, JoinHandle<()>)> {
    let channel = jump_session
        .channel_direct_tcpip(host, port, Some(("127.0.0.1", 22)))
        .await
        .ssh()?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _keep_jump = jump_session;
        match listener.accept().await {
            Ok((mut sock, _)) => {
                let mut channel = channel;
                if let Err(err) = tokio::io::copy_bidirectional(&mut channel, &mut sock).await {
                    debug!("ProxyJump tunnel closed: {err}");
                }
            }
            Err(err) => debug!("ProxyJump local accept failed: {err}"),
        }
    });

    let stream = timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(local_addr))
        .await
        .map_err(|_| NetconfClientError::new("timeout connecting through ProxyJump"))?
        .map_err(NetconfClientError::from)?;
    Ok((stream, handle))
}

async fn handshake_and_auth(
    stream: TcpStream,
    target: Target<'_>,
) -> NetconfClientResult<AsyncSession<TcpStream>> {
    let mut configuration = SessionConfiguration::new();
    configuration.set_timeout(SSH_TIMEOUT_MS);
    apply_session_configuration(&mut configuration, target.session);
    let mut session = AsyncSession::new(stream, configuration).ssh()?;
    apply_method_prefs(&session, target.session).await?;
    session.handshake().await.ssh()?;
    verify_host_key(&session, target.host, target.port, target.host_key)?;
    authenticate(&session, target.username, target.auth).await?;
    if !session.authenticated() {
        return Err(NetconfClientError::new(format!(
            "SSH authentication failed for {}@{}:{}",
            target.username, target.host, target.port
        )));
    }
    Ok(session)
}

fn apply_session_configuration(configuration: &mut SessionConfiguration, opts: &SshSessionOpts) {
    if let Some(compress) = opts.compression {
        configuration.set_compress(compress);
    }
    if let Some(interval) = opts.keepalive {
        configuration.set_keepalive(true, interval.as_secs() as u32);
    }
}

async fn apply_method_prefs(
    session: &AsyncSession<TcpStream>,
    opts: &SshSessionOpts,
) -> NetconfClientResult<()> {
    if let Some(kex) = opts.kex.as_deref() {
        session
            .method_pref(ssh2::MethodType::Kex, kex)
            .await
            .ssh()?;
    }
    if let Some(host_key) = opts.host_key_algs.as_deref() {
        session
            .method_pref(ssh2::MethodType::HostKey, host_key)
            .await
            .ssh()?;
    }
    if let Some(ciphers) = opts.ciphers.as_deref() {
        session
            .method_pref(ssh2::MethodType::CryptCs, ciphers)
            .await
            .ssh()?;
    }
    if let Some(macs) = opts.macs.as_deref() {
        session
            .method_pref(ssh2::MethodType::MacCs, macs)
            .await
            .ssh()?;
        session
            .method_pref(ssh2::MethodType::MacSc, macs)
            .await
            .ssh()?;
    }
    Ok(())
}

fn verify_host_key(
    session: &AsyncSession<TcpStream>,
    host: &str,
    port: u16,
    policy: &HostKeyPolicy,
) -> NetconfClientResult<()> {
    match policy {
        HostKeyPolicy::KnownHosts(path) => {
            return check_known_hosts(session, host, port, path, false);
        }
        HostKeyPolicy::AcceptNew(path) => {
            return check_known_hosts(session, host, port, path, true);
        }
        _ => {}
    }
    let fingerprint = host_key_fingerprint(session)?;
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
    session: &AsyncSession<TcpStream>,
    host: &str,
    port: u16,
    path: &Path,
    accept_new: bool,
) -> NetconfClientResult<()> {
    let fingerprint = host_key_fingerprint(session)?;
    let (key, key_type) = session
        .host_key()
        .ok_or_else(|| NetconfClientError::new("server did not provide a host key"))?;
    let mut known = session.known_hosts().ssh()?;
    if path.exists() {
        known
            .read_file(path, ssh2::KnownHostFileKind::OpenSSH)
            .map_err(|err| {
                NetconfClientError::new(format!(
                    "failed to read known_hosts {}: {err}",
                    path.display()
                ))
            })?;
    } else if !accept_new {
        return Err(NetconfClientError::new(format!(
            "failed to read known_hosts {}: file not found",
            path.display()
        )));
    }
    match known_hosts_action(known.check_port(host, port, key), accept_new) {
        KnownHostsAction::Allow => Ok(()),
        KnownHostsAction::Pin => {
            pin_new_host(&mut known, path, host, port, key, key_type, &fingerprint)
        }
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

fn known_hosts_action(result: ssh2::CheckResult, accept_new: bool) -> KnownHostsAction {
    match result {
        ssh2::CheckResult::Match => KnownHostsAction::Allow,
        ssh2::CheckResult::NotFound if accept_new => KnownHostsAction::Pin,
        other => KnownHostsAction::Reject(known_hosts_reason(other)),
    }
}

fn pin_new_host(
    known: &mut ssh2::KnownHosts,
    path: &Path,
    host: &str,
    port: u16,
    key: &[u8],
    key_type: ssh2::HostKeyType,
    fingerprint: &str,
) -> NetconfClientResult<()> {
    let spec = known_hosts_host_spec(host, port);
    known
        .add(&spec, key, host, key_type.into())
        .map_err(|err| {
            NetconfClientError::new(format!(
                "failed to record host key for {host} in {}: {err}",
                path.display()
            ))
        })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    known
        .write_file(path, ssh2::KnownHostFileKind::OpenSSH)
        .map_err(|err| {
            NetconfClientError::new(format!(
                "failed to write known_hosts {}: {err}",
                path.display()
            ))
        })?;
    warn!(
        "accepted new SSH host key for {host} ({fingerprint}); stored in {}",
        path.display()
    );
    Ok(())
}

fn known_hosts_host_spec(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn known_hosts_reason(result: ssh2::CheckResult) -> &'static str {
    match result {
        ssh2::CheckResult::Match => "matched",
        ssh2::CheckResult::Mismatch => "key does not match known_hosts",
        ssh2::CheckResult::NotFound => "host not in known_hosts",
        ssh2::CheckResult::Failure => "known_hosts check failed",
    }
}

fn host_key_fingerprint(session: &AsyncSession<TcpStream>) -> NetconfClientResult<String> {
    let hash = session
        .host_key_hash(ssh2::HashType::Sha256)
        .ok_or_else(|| NetconfClientError::new("server did not provide a host key"))?;
    Ok(format!("SHA256:{}", base64_nopad(hash)))
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
    session: &AsyncSession<TcpStream>,
    username: &str,
    auth: &SshAuth,
) -> NetconfClientResult<()> {
    match auth {
        SshAuth::Password(password) => {
            session
                .userauth_password(username, password.as_str())
                .await
                .ssh()?;
        }
        SshAuth::Agent => {
            session.userauth_agent_with_try_next(username).await.ssh()?;
        }
        SshAuth::KeyFile { path, passphrase } => {
            session
                .userauth_pubkey_file(
                    username,
                    None::<&Path>,
                    path,
                    passphrase.as_deref().map(String::as_str),
                )
                .await
                .ssh()?;
        }
    }
    Ok(())
}

fn ssh_err(err: async_ssh2_lite::Error) -> NetconfClientError {
    match err {
        async_ssh2_lite::Error::Io(io) => NetconfClientError::Io(io),
        other => {
            if other.as_ssh2().is_some_and(is_ssh2_eof) {
                return NetconfClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    other.to_string(),
                ));
            }
            let message = match other.as_ssh2() {
                Some(ssh2) => ssh2.to_string(),
                None => match other.as_other() {
                    Some(inner) => inner.to_string(),
                    None => other.to_string(),
                },
            };
            NetconfClientError::Ssh(message)
        }
    }
}

fn is_ssh2_eof(err: &ssh2::Error) -> bool {
    let msg = err.message();
    msg.eq_ignore_ascii_case("end of file")
        || msg.eq_ignore_ascii_case("eof sent")
        || msg.eq_ignore_ascii_case("socket disconnected")
}

trait MapSsh<T> {
    fn ssh(self) -> NetconfClientResult<T>;
}

impl<T> MapSsh<T> for Result<T, async_ssh2_lite::Error> {
    fn ssh(self) -> NetconfClientResult<T> {
        self.map_err(ssh_err)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_nopad(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(B64[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    if rem.len() == 1 {
        let n = u32::from(rem[0]) << 16;
        out.push(B64[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
    } else if rem.len() == 2 {
        let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
        out.push(B64[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64[((n >> 6) & 0x3f) as usize] as char);
    }
    out
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
    fn known_hosts_reason_maps_libssh2_results() {
        assert_eq!(
            known_hosts_reason(ssh2::CheckResult::Mismatch),
            "key does not match known_hosts"
        );
        assert_eq!(
            known_hosts_reason(ssh2::CheckResult::NotFound),
            "host not in known_hosts"
        );
        assert_eq!(
            known_hosts_reason(ssh2::CheckResult::Failure),
            "known_hosts check failed"
        );
    }

    #[test]
    fn accept_new_pins_unknown_and_rejects_mismatch() {
        assert_eq!(
            known_hosts_action(ssh2::CheckResult::NotFound, true),
            KnownHostsAction::Pin
        );
        assert_eq!(
            known_hosts_action(ssh2::CheckResult::Mismatch, true),
            KnownHostsAction::Reject("key does not match known_hosts")
        );
        assert_eq!(
            known_hosts_action(ssh2::CheckResult::NotFound, false),
            KnownHostsAction::Reject("host not in known_hosts")
        );
        assert_eq!(
            known_hosts_action(ssh2::CheckResult::Match, true),
            KnownHostsAction::Allow
        );
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
        let io = ssh_err(async_ssh2_lite::Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "pipe closed",
        )));
        assert!(
            matches!(&io, NetconfClientError::Io(err) if err.kind() == std::io::ErrorKind::UnexpectedEof),
            "{io:?}"
        );
        let other = ssh_err(async_ssh2_lite::Error::Other("auth failed".into()));
        assert!(
            matches!(&other, NetconfClientError::Ssh(msg) if msg.contains("auth failed")),
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
    fn base64_nopad_encodes_sha256_of_empty() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = hex_bytes("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(
            base64_nopad(&hash),
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
    }

    #[test]
    fn ssh_auth_debug_redacts_secrets() {
        let password = format!("{:?}", SshAuth::password("secret"));
        assert!(!password.contains("secret"), "{password}");
        let key = format!("{:?}", SshAuth::key_file("/tmp/id", Some("s3cret".into())));
        assert!(!key.contains("s3cret"), "{key}");
        assert!(key.contains("/tmp/id"), "{key}");
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }
}
