use crate::commands::builtin::{arg, value_of, xml_file_from_args};
use crate::config::Config;
use clap::{Command, ValueHint};
use log::{error, info};
use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientResult;
use netconf_async::message::{CopySource, Datastore};
use std::str::FromStr;

pub fn cli() -> Command {
    Command::new("copy")
        .about("Execute copy-config rpc")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            arg(
                "source",
                "Source datastore or URL (running, candidate, startup, or file|http|ftp URL)",
                false,
                Some('s'),
                None,
                None,
                None,
            )
            .required_unless_present("config")
            .conflicts_with("config"),
            arg(
                "target",
                "Target datastore or URL (running, candidate, startup, or file|http|ftp URL)",
                true,
                Some('t'),
                None,
                None,
                None,
            ),
            arg(
                "config",
                "File containing a complete config to use as source",
                false,
                Some('c'),
                None,
                Some(ValueHint::FilePath),
                None,
            ),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let target = Datastore::from_str(value_of::<String>("target", &cfg.args))?;
    let source = if let Some(xml) = xml_file_from_args("config", &cfg.args)? {
        CopySource::Config(xml)
    } else {
        CopySource::from(Datastore::from_str(value_of::<String>(
            "source", &cfg.args,
        ))?)
    };

    match conn.copy_config(source, target).await {
        Ok(resp) => {
            info!("Response:\n{}", resp);
        }
        Err(err) => {
            error!("Copy error: {}", err);
        }
    };
    Ok(())
}
