use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::commands::builtin::{value_of, value_of_if_exists};
use crate::config::{self, Host};
use clap::{Arg, ArgAction, ArgMatches, Command};
use log::info;
use netconf_async::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::transport::ssh::SshTransport;
use netconf_mcp::{ConnectParams, DeviceConnect, McpConfig, McpTransport};
use ssh2_config::{DefaultAlgorithms, HostParams};

pub fn cli() -> Command {
    Command::new("mcp")
        .about("Start an MCP server for NETCONF operations")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            Arg::new("stdio")
                .long("stdio")
                .action(ArgAction::SetTrue)
                .help("Use stdio transport (default)"),
            Arg::new("http")
                .long("http")
                .action(ArgAction::SetTrue)
                .help("Use streamable HTTP transport"),
            Arg::new("port")
                .long("port")
                .value_parser(clap::value_parser!(u16))
                .default_value("8080")
                .requires("http")
                .help("Port for HTTP server"),
            Arg::new("bind")
                .long("bind")
                .default_value("127.0.0.1")
                .requires("http")
                .help("Bind address for HTTP server (default 127.0.0.1)"),
            Arg::new("read-only")
                .long("read-only")
                .action(ArgAction::SetTrue)
                .help("Hide write tools (rpc, edit_config, copy_config, commit)"),
            Arg::new("allowed-subnet")
                .long("allowed-subnet")
                .action(ArgAction::Append)
                .help("Allowed IP subnets in CIDR notation (repeatable)"),
        ])
        .mut_arg("stdio", |arg| arg.conflicts_with("http"))
}

pub async fn exec(args: &ArgMatches) -> NetconfClientResult<()> {
    let http = *value_of::<bool>("http", args);
    let read_only = *value_of::<bool>("read-only", args);
    let port = *value_of::<u16>("port", args);
    let bind = value_of::<String>("bind", args);
    let subnets: Vec<String> = args
        .get_many::<String>("allowed-subnet")
        .unwrap_or_default()
        .cloned()
        .collect();

    let transport = if http {
        let ip: IpAddr = bind
            .parse()
            .map_err(|err| NetconfClientError::new(format!("invalid --bind {bind:?}: {err}")))?;
        if !ip.is_loopback() {
            return Err(NetconfClientError::new(format!(
                "--bind {bind} is not loopback; HTTP is localhost-only"
            )));
        }
        McpTransport::Http {
            bind: SocketAddr::new(ip, port),
        }
    } else {
        McpTransport::Stdio
    };

    let mut config = McpConfig::stdio();
    config.transport = transport;
    config.read_only = read_only;
    config = config
        .with_allowed_subnets(subnets)
        .map_err(NetconfClientError::new)?;

    let username = value_of_if_exists::<String>("username", args).cloned();
    let password = value_of_if_exists::<String>("password", args).cloned();
    let timeout = value_of_if_exists::<u64>("timeout", args).copied();
    let strict = value_of_if_exists::<String>("strict-host-key-checking", args)
        .and_then(|value| config::parse_host_key_check(value));

    if read_only {
        info!("MCP read-only mode");
    }
    match &config.transport {
        McpTransport::Stdio => info!("MCP stdio"),
        McpTransport::Http { bind } => info!("MCP HTTP on {bind}"),
    }

    let connector = CliConnector {
        username,
        password,
        timeout,
        ssh_config: config::load_ssh_config(),
        strict_host_key: strict,
    };
    netconf_mcp::serve(config, connector)
        .await
        .map_err(|err| NetconfClientError::new(err.to_string()))
}

struct CliConnector {
    username: Option<String>,
    password: Option<String>,
    timeout: Option<u64>,
    ssh_config: Option<ssh2_config::SshConfig>,
    strict_host_key: Option<config::HostKeyCheck>,
}

#[async_trait::async_trait]
impl DeviceConnect for CliConnector {
    async fn connect(&self, params: ConnectParams) -> NetconfClientResult<Connection> {
        let query = crate::inventory::host_key(&params.host);
        let host_params = if let Some(ssh_config) = &self.ssh_config {
            ssh_config.query(&query)
        } else {
            HostParams::new(&DefaultAlgorithms::default())
        };
        let username = params.username.clone().or_else(|| self.username.clone());
        let password = params.password.clone().or_else(|| self.password.clone());
        let host = Host::new(
            &params.host,
            &username,
            &password,
            host_params,
            830,
            self.ssh_config.clone(),
        )?
        .strict_host_key(self.strict_host_key);
        // Pin after HostName expansion so an ssh_config alias cannot jump
        // outside the allow-list. Dial the pinned IP; keep the logical name
        // for known_hosts. Jump hops are the path, not the target.
        let dial = params
            .allowed_subnets
            .pin(&host.address)
            .map_err(NetconfClientError::new)?;
        let mut ssh_config = host.ssh_transport_config()?;
        if dial != host.address {
            ssh_config = ssh_config.connect_to(dial);
        }
        let transport = SshTransport::connect(ssh_config).await?;
        let mut connection = Connection::new(transport).await?;
        let timeout = params.timeout.or(self.timeout);
        if let Some(secs) = timeout {
            connection.set_timeout(Some(Duration::from_secs(secs)));
        }
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use crate::cli;

    #[test]
    fn mcp_does_not_require_host() {
        let matches = cli::cli()
            .try_get_matches_from(["netconf", "mcp", "--help"])
            .err();
        // --help is an error in clap that prints help; parsing without host must work
        let _ = matches;
        let parsed = cli::cli().try_get_matches_from(["netconf", "mcp", "--read-only"]);
        assert!(parsed.is_ok(), "{parsed:?}");
    }

    #[test]
    fn mcp_http_flags() {
        let parsed = cli::cli()
            .try_get_matches_from([
                "netconf",
                "mcp",
                "--http",
                "--bind",
                "127.0.0.1",
                "--port",
                "9090",
                "--allowed-subnet",
                "192.0.2.0/24",
            ])
            .unwrap();
        let (cmd, args) = parsed.subcommand().unwrap();
        assert_eq!(cmd, "mcp");
        assert!(*args.get_one::<bool>("http").unwrap());
        assert_eq!(*args.get_one::<u16>("port").unwrap(), 9090);
    }
}
