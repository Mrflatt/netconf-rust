use crate::commands::builtin::{builtin, builtin_exec};
use crate::config::{CliConfig, Host};
use clap::{
    arg, crate_authors, crate_description, crate_name, crate_version, Arg, ArgAction, Command,
};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use log::{debug, error, info};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::transport::ssh::SSHTransport;
use ssh2_config::HostParams;
use std::time::Instant;
use tokio::task::JoinHandle;

pub async fn exec(cmd: String, cfg: CliConfig) -> NetconfClientResult<()> {
    let hosts = &cfg.inner.addresses;
    let mut futures = FuturesUnordered::new();
    for addr in hosts {
        let params = if let Some(ssh_config) = &cfg.inner.ssh_config {
            ssh_config.query(addr)
        } else {
            HostParams::default()
        };
        let host = Host::new(addr, &cfg.inner.username, &cfg.inner.password, params)?;
        let start_time = Instant::now();
        let cmd_clone = cmd.clone();
        let cfg_clone = cfg.clone();
        let handle: JoinHandle<NetconfClientResult<()>> = tokio::spawn(async move {
            let session = host.connect_ssh().await?;
            let ssh_transport = SSHTransport::new_with_session(session).await?;
            let mut connection = Connection::new(ssh_transport).await?;
            info!(target: &host.address, "Connected to host");
            debug!(
                target: &host.address,
                "Started Netconf session with session-id: {}",
                connection.session_id()
            );

            if let Some(result) = builtin_exec(&cmd_clone, &mut connection, &cfg_clone.inner).await
            {
                match result {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e),
                }
            } else {
                Err(NetconfClientError::Anyhow(anyhow::Error::msg(
                    "Unknown command",
                )))
            }?;

            info!(target: &host.address, "Operation took: {:.3}s", start_time.elapsed().as_secs_f32());
            connection.close_session().await?;
            Ok(())
        });
        futures.push(handle);
    }

    while let Some(handle) = futures.next().await {
        match handle {
            Ok(result) => {
                if let Err(err) = result {
                    error!("Task failed with error: {}", err);
                } else {
                    debug!("Task completed successfully")
                }
            }
            Err(err) => error!("Task failed: {}", err),
        }
    }
    Ok(())
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
    <cyan,bold>rpc</>               Execute raw rpc
    <cyan,bold>notification</>      Start netconf notification listener

See '<cyan,bold>netconf help</> <cyan><<command>></>' for more information on a specific command.\n",
        ))
        .args([
            arg!(-v --verbose ... "Use verbose output (-vv to log all rpc responses, -vvv to print also rpc requests)")
                .global(true),
            arg!(-q --quiet "Disable logging completely")
                .global(true),
            global_opt("host", "Username for netconf connection")
                .env("NETCONF_HOST")
                .action(ArgAction::Append)
                .value_delimiter(','),
            global_opt("username", "Username for netconf connection")
                .env("NETCONF_USERNAME"),
            global_opt("password", "Username for netconf connection")
                .env("NETCONF_PASSWORD")
                .hide_env(true),
        ])
        .subcommands(builtin())
}

fn global_opt(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).help(help).long(name).global(true)
}

#[test]
fn verify_cli() {
    cli().debug_assert();
}
