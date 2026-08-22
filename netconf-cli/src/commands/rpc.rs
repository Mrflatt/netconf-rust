use crate::commands::builtin::{arg, xml_inputs_from_args};
use crate::config::Config;
use clap::{Command, ValueHint};
use log::{debug, error, info};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};

pub fn cli() -> Command {
    Command::new("rpc")
        .about("Execute raw rpc")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([arg(
            "file",
            "RPC XML, file or directory, @path, or '-' for stdin (executed in name order)",
            true,
            Some('f'),
            None,
            Some(ValueHint::AnyPath),
            None,
        )])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    match run(cfg, conn).await {
        Ok(()) => Ok(()),
        Err(err) => {
            error!("Rpc error: {}", err);
            Err(err)
        }
    }
}

async fn run(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let files = xml_inputs_from_args("file", cfg)?;
    if files.is_empty() {
        return Err(NetconfClientError::new("rpc file required".to_string()));
    }

    for file in &files {
        debug!("Executing {}", file.name);
        let resp = conn.raw_rpc(&file.content).await?;
        info!("Response from {}:\n{}", file.name, resp);
    }
    Ok(())
}
