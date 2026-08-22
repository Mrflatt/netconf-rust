use crate::commands::builtin::value_of;
use config::CliConfig;
use env_logger::{Builder, Target};
use log::LevelFilter;
use netconf_async::error::NetconfClientResult;

mod cli;
mod commands;
mod config;
mod output;
mod update;

fn quiet_libraries(builder: &mut Builder) {
    builder.filter_module("netconf_async", LevelFilter::Off);
    builder.filter_module("russh", LevelFilter::Off);
    builder.filter_module("ssh2_config", LevelFilter::Off);
}

fn init_logging(verbosity: &u8) {
    let mut builder = Builder::new();
    match verbosity {
        1 => {
            builder.filter_level(LevelFilter::Debug);
            quiet_libraries(&mut builder);
        }
        2 => {
            builder.filter_level(LevelFilter::Debug);
            builder.filter_module("netconf_async::framer::async_framer", LevelFilter::Off);
            builder.filter_module("netconf_async::connection", LevelFilter::Debug);
        }
        3 => {
            builder.filter_level(LevelFilter::Debug);
            builder.filter_module("netconf_async", LevelFilter::Debug);
        }
        _ => {
            builder.filter_level(LevelFilter::Info);
            quiet_libraries(&mut builder);
        }
    };
    builder.target(Target::Stderr);
    builder.init();
}

#[tokio::main]
async fn main() -> NetconfClientResult<()> {
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
            } else {
                let cli_config = CliConfig::new(args)?;
                cli::exec(cmd.to_owned(), cli_config).await?;
            }
        }
        _ => {
            cli::cli().print_help()?;
        }
    }
    Ok(())
}
