use crate::commands::builtin::value_of;
use config::CliConfig;
use env_logger::{Builder, Target};
use log::LevelFilter;
use netconf_async::error::NetconfClientResult;
use std::process::ExitCode;

mod cli;
mod commands;
mod config;
mod inventory;
mod output;
mod template;
mod update;

fn quiet_third_party(builder: &mut Builder) {
    builder.filter_module("russh", LevelFilter::Off);
    builder.filter_module("ssh2_config", LevelFilter::Off);
}

fn our_trace(builder: &mut Builder) {
    builder.filter_module("netconf_cli", LevelFilter::Trace);
    builder.filter_module("netconf_async", LevelFilter::Trace);
}

fn init_logging(verbosity: &u8) {
    let mut builder = Builder::new();
    match verbosity {
        0 => {
            builder.filter_level(LevelFilter::Info);
            quiet_third_party(&mut builder);
        }
        1 => {
            builder.filter_level(LevelFilter::Debug);
            quiet_third_party(&mut builder);
        }
        2 => {
            builder.filter_level(LevelFilter::Debug);
            our_trace(&mut builder);
            quiet_third_party(&mut builder);
        }
        3 => {
            builder.filter_level(LevelFilter::Debug);
            our_trace(&mut builder);
        }
        _ => {
            builder.filter_level(LevelFilter::Debug);
            our_trace(&mut builder);
            builder.filter_module("russh", LevelFilter::Trace);
            builder.filter_module("ssh2_config", LevelFilter::Trace);
        }
    }
    builder.target(Target::Stderr);
    builder.init();
}

#[tokio::main]
async fn main() -> ExitCode {
    match try_main().await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn try_main() -> NetconfClientResult<bool> {
    let mut args = cli::cli().get_matches();
    let verbosity = value_of::<u8>("verbose", &args);
    let disable_logging = value_of::<bool>("quiet", &args);
    if !disable_logging {
        init_logging(verbosity);
    }

    match args.remove_subcommand() {
        Some((cmd, args)) => {
            if cmd == "update" {
                commands::update::exec(&args).await?;
                Ok(true)
            } else if cmd == "mcp" {
                commands::mcp::exec(&args).await?;
                Ok(true)
            } else {
                let cli_config = CliConfig::new(args)?;
                cli::exec(cmd.to_owned(), cli_config).await
            }
        }
        _ => {
            cli::cli().print_help()?;
            Ok(true)
        }
    }
}
