use crate::commands::builtin::{builtin, builtin_exec};
use crate::config::{CliConfig, Host};
use clap::{
    Arg, ArgAction, Command, arg, crate_authors, crate_description, crate_name, crate_version,
};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use log::{debug, error, info};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::transport::ssh::{JumpPool, SSHTransport, SshJump};
use ssh2_config::{DefaultAlgorithms, HostParams};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinHandle;

pub async fn exec(cmd: String, cfg: CliConfig) -> NetconfClientResult<()> {
    let hosts = &cfg.inner.addresses;
    let sem = Arc::new(tokio::sync::Semaphore::new(cfg.inner.parallel));
    let mut prepared = Vec::new();
    for addr in hosts {
        let params = if let Some(ssh_config) = &cfg.inner.ssh_config {
            ssh_config.query(addr)
        } else {
            HostParams::new(&DefaultAlgorithms::default())
        };
        let host = Host::new(
            addr,
            &cfg.inner.username,
            &cfg.inner.password,
            params,
            830,
            cfg.inner.ssh_config.clone(),
        )?
        .strict_host_key(cfg.inner.strict_host_key);
        let ssh_config = host.ssh_transport_config()?;
        prepared.push((host, ssh_config, addr.clone()));
    }
    let mut pools = Vec::new();
    let mut targets = Vec::new();
    for (host, ssh_config, addr) in prepared {
        let pool = shared_jump_pool(ssh_config.jumps(), &mut pools)?;
        targets.push((host, ssh_config, addr, pool));
    }
    for (jumps, pool) in &pools {
        let n = targets
            .iter()
            .filter(|(_, _, _, target_pool)| {
                target_pool
                    .as_ref()
                    .is_some_and(|target| Arc::ptr_eq(target, pool))
            })
            .count();
        if n > 1 {
            info!(
                "Sharing ProxyJump {} across {n} hosts",
                format_jump_hops(jumps)
            );
        }
    }
    let mut futures = FuturesUnordered::new();
    for (host, ssh_config, addr, pool) in targets {
        let start_time = Instant::now();
        let cmd_clone = cmd.clone();
        let cfg_clone = cfg.clone();
        let sem = sem.clone();
        let handle: JoinHandle<NetconfClientResult<()>> = tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.map_err(|err| {
                NetconfClientError::new(format!("failed to acquire host slot: {err}"))
            })?;
            let ssh_transport = match pool {
                Some(pool) => pool.connect_device(ssh_config).await?,
                None => SSHTransport::connect(ssh_config).await?,
            };
            let mut connection = Connection::new(ssh_transport).await?;
            if let Some(timeout) = cfg_clone.inner.timeout {
                connection.set_timeout(Some(timeout));
            }
            info!(target: &host.address, "Connected to host");
            debug!(
                target: &host.address,
                "Started Netconf session with session-id: {}",
                connection
                    .session_id()
                    .map_or_else(|| "unknown".to_string(), |id| id.to_string())
            );

            if let Some(result) =
                builtin_exec(&cmd_clone, &mut connection, &cfg_clone.inner, &addr).await
            {
                result
            } else {
                Err(NetconfClientError::new("Unknown command"))
            }?;

            info!(target: &host.address, "Operation took: {:.3}s", start_time.elapsed().as_secs_f32());
            connection.close_session().await?;
            Ok(())
        });
        futures.push(handle);
    }

    let mut first_err = None;
    while let Some(handle) = futures.next().await {
        match handle {
            Ok(Ok(())) => debug!("Task completed successfully"),
            Ok(Err(err)) => {
                error!("Task failed with error: {}", err);
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
            Err(err) => {
                error!("Task failed: {}", err);
                if first_err.is_none() {
                    first_err = Some(NetconfClientError::new(format!("Task failed: {err}")));
                }
            }
        }
    }
    match first_err {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

pub fn cli() -> Command {
    Command::new(crate_name!())
        .author(crate_authors!("\n"))
        .about(crate_description!())
        .version(crate_version!())
        .long_version(crate_version!())
        .arg_required_else_help(true)
        .allow_external_subcommands(false)
        .bin_name("netconf")
        .display_name("netconf")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Author:</> {author}

<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}

<green,bold>Commands:</>
    <cyan,bold>get</>               Execute get rpc
    <cyan,bold>get-config</>        Execute get-config rpc
    <cyan,bold>edit</>              Execute edit-config rpc
    <cyan,bold>copy</>              Execute copy-config rpc
    <cyan,bold>commit</>            Commit candidate or confirm/cancel persist commit
    <cyan,bold>rpc</>               Execute raw rpc
    <cyan,bold>notification</>      Start netconf notification listener
    <cyan,bold>update</>            Update netconf-cli from GitHub releases

See '<cyan,bold>netconf help</> <cyan><<command>></>' for more information on a specific command.\n",
        ))
        .args([
            arg!(-v --verbose ... "Use verbose output (-vv to log all rpc responses, -vvv to print also rpc requests)")
                .global(true),
            arg!(-q --quiet "Disable logging completely")
                .global(true),
            global_opt("host", "NETCONF host (repeat or comma-separate for fan-out)")
                .env("NETCONF_HOST")
                .action(ArgAction::Append)
                .value_delimiter(','),
            global_opt("username", "Username for NETCONF connection")
                .env("NETCONF_USERNAME"),
            global_opt("password", "Password for NETCONF connection")
                .env("NETCONF_PASSWORD")
                .hide_env(true),
            Arg::new("timeout")
                .long("timeout")
                .help("Per-RPC timeout in seconds")
                .value_parser(clap::value_parser!(u64).range(1..))
                .global(true),
            Arg::new("parallel")
                .long("parallel")
                .help("Max concurrent hosts / jump channels (default: number of CPUs)")
                .value_parser(clap::value_parser!(u64).range(1..))
                .global(true),
            Arg::new("format")
                .long("format")
                .help("Reply format: xml (default) or json, plus pretty, unescape (comma-separated)")
                .value_parser(crate::output::parse_format)
                .default_value("xml")
                .global(true),
            Arg::new("output-dir")
                .long("output-dir")
                .help("Write each host reply to DIR/{host}.{xml|json} instead of stdout")
                .value_hint(clap::ValueHint::DirPath)
                .global(true),
            Arg::new("strict-host-key-checking")
                .long("strict-host-key-checking")
                .help("Host-key check: accept-new (default), yes, or no")
                .value_parser(["yes", "no", "accept-new"])
                .num_args(0..=1)
                .default_missing_value("yes")
                .global(true),
        ])
        .subcommands(builtin())
        .subcommand(crate::commands::update::cli())
}

fn global_opt(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).help(help).long(name).global(true)
}

fn format_jump_hops(jumps: &[SshJump]) -> String {
    jumps
        .iter()
        .map(|jump| format!("{}:{}", jump.host(), jump.port()))
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn shared_jump_pool(
    jumps: &[SshJump],
    pools: &mut Vec<(Vec<SshJump>, Arc<JumpPool>)>,
) -> NetconfClientResult<Option<Arc<JumpPool>>> {
    if jumps.is_empty() {
        return Ok(None);
    }
    if let Some((_, pool)) = pools.iter().find(|(key, _)| key.as_slice() == jumps) {
        return Ok(Some(pool.clone()));
    }
    let pool = Arc::new(JumpPool::new(jumps.to_vec())?);
    pools.push((jumps.to_vec(), pool.clone()));
    Ok(Some(pool))
}

#[test]
fn verify_cli() {
    cli().debug_assert();
}

#[test]
fn timeout_and_parallel_flags() {
    use crate::config::CliConfig;
    use std::time::Duration;

    let mut matches = cli()
        .try_get_matches_from([
            "netconf",
            "--host",
            "192.0.2.10",
            "--timeout",
            "15",
            "--parallel",
            "3",
            "get-config",
        ])
        .unwrap();
    let (_, args) = matches.remove_subcommand().unwrap();
    let cfg = CliConfig::new(args).unwrap();
    assert_eq!(cfg.inner.timeout, Some(Duration::from_secs(15)));
    assert_eq!(cfg.inner.parallel, 3);
}

#[test]
fn parallel_defaults_to_cpu_count() {
    use crate::config::CliConfig;

    let mut matches = cli()
        .try_get_matches_from(["netconf", "--host", "192.0.2.10", "get-config"])
        .unwrap();
    let (_, args) = matches.remove_subcommand().unwrap();
    let cfg = CliConfig::new(args).unwrap();
    assert!(cfg.inner.parallel >= 1, "{}", cfg.inner.parallel);
}

#[test]
fn format_and_output_dir_flags() {
    use crate::config::CliConfig;
    use crate::output::{Format, FormatKind};

    let mut matches = cli()
        .try_get_matches_from([
            "netconf",
            "--host",
            "192.0.2.10",
            "--format",
            "json,pretty",
            "--output-dir",
            "/tmp/replies",
            "get-config",
        ])
        .unwrap();
    let (_, args) = matches.remove_subcommand().unwrap();
    let cfg = CliConfig::new(args).unwrap();
    assert_eq!(
        cfg.inner.output.format_for_test(),
        &Format {
            kind: FormatKind::Json,
            pretty: true,
            unescape: false,
        }
    );
    assert_eq!(
        cfg.inner.output.dir_for_test().unwrap().as_os_str(),
        "/tmp/replies"
    );
}

#[test]
fn shared_jump_pool_groups_identical_chains() {
    use netconf_async::transport::ssh::SshAuth;

    let jump = SshJump::new("jump", 22, "jump-user", SshAuth::Agent);
    let other = SshJump::new("other", 22, "jump-user", SshAuth::Agent);
    let mut pools = Vec::new();

    assert!(shared_jump_pool(&[], &mut pools).unwrap().is_none());
    let a = shared_jump_pool(std::slice::from_ref(&jump), &mut pools)
        .unwrap()
        .unwrap();
    let b = shared_jump_pool(std::slice::from_ref(&jump), &mut pools)
        .unwrap()
        .unwrap();
    let c = shared_jump_pool(std::slice::from_ref(&other), &mut pools)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(pools.len(), 2);
    assert_eq!(format_jump_hops(&[jump]), "jump:22");
}
