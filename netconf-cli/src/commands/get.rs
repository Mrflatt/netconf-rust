use crate::commands::builtin::{filter_from_args, value_of_if_exists};
use crate::config::Config;
use clap::{Arg, Command, ValueHint};
use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientResult;
use netconf_async::message::WithDefaultsValue;
use std::str::FromStr;

pub fn cli() -> Command {
    Command::new("get")
        .about("Execute get rpc")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            Arg::new("filter")
                .help("Subtree filter XML, file path, @file, or '-' for stdin (required; use get-config without filter)")
                .short('f')
                .long("filter")
                .required(true)
                .value_hint(ValueHint::FilePath),
            Arg::new("defaults")
                .help("With-defaults option")
                .long("with-defaults")
                .value_parser(["report-all", "report-all-tagged", "trim", "explicit"])
                .env("NETCONF_WITH_DEFAULTS"),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection, host: &str) -> NetconfClientResult<()> {
    let with_defaults = value_of_if_exists::<String>("defaults", &cfg.args)
        .map(|value| WithDefaultsValue::from_str(value).unwrap());
    let filter = filter_from_args(cfg)?;
    let resp = conn.get(filter, with_defaults).await?;
    cfg.output.emit(host, &resp)
}
