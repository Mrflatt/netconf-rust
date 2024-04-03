use crate::commands::builtin::{arg, value_of, value_of_if_exists};
use crate::config::Config;
use clap::{Command, ValueHint};
use log::{error, info};
use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientResult;
use netconf_async::message::{Datastore, WithDefaultsValue};
use std::str::FromStr;

pub fn cli() -> Command {
    Command::new("get-config")
        .about("Execute get-config rpc")
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
                "Datastore to get config",
                false,
                Some('s'),
                Some("running"),
                None,
                ["running", "startup", "candidate"],
            ),
            arg(
                "filter",
                "File containing filters",
                false,
                Some('f'),
                None,
                Some(ValueHint::FilePath),
                None,
            ),
            arg(
                "with-defaults",
                "With-defaults option",
                false,
                None,
                None,
                None,
                ["report-all", "report-all-tagged", "trim", "explicit"],
            )
            .env("NETCONF_WITH_DEFAULTS"),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let source = value_of::<String>("source", &cfg.args);
    let with_defaults = value_of_if_exists::<String>("with-defaults", &cfg.args)
        .map(|value| WithDefaultsValue::from_str(value).unwrap());
    let source = Datastore::from_str(source)?;
    match conn.get_config(source, None, with_defaults).await {
        Ok(resp) => {
            info!("Response:\n{}", resp);
        }
        Err(err) => {
            error!("Get error: {}", err);
        }
    };
    Ok(())
}
