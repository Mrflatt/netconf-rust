use crate::commands::builtin::{arg, value_of, value_of_if_exists};
use crate::config::Config;
use clap::{Arg, ArgAction, Command};
use log::{debug, info};
use netconf_async::STARTUP_CAP;
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::message::{CopySource, Datastore};

pub fn cli() -> Command {
    Command::new("commit")
        .about("Commit candidate, or confirm/cancel a persist confirmed-commit")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            arg(
                "id",
                "Persist-id from a previous `edit --confirmed`",
                false,
                None,
                None,
                None,
                None,
            ),
            Arg::new("cancel")
                .long("cancel")
                .help("Cancel a persist confirmed-commit and discard candidate")
                .action(ArgAction::SetTrue)
                .requires("id"),
            Arg::new("no-copy")
                .long("no-copy")
                .help("Do not copy running to startup after commit")
                .action(ArgAction::SetTrue)
                .conflicts_with("cancel"),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let persist_id = value_of_if_exists::<String>("id", &cfg.args);
    let cancel = *value_of::<bool>("cancel", &cfg.args);
    let no_copy = *value_of::<bool>("no-copy", &cfg.args);

    if cancel {
        let persist_id = persist_id
            .ok_or_else(|| NetconfClientError::new("--id is required with --cancel".to_string()))?;
        info!("Cancelling confirmed commit persist-id: {persist_id}");
        conn.cancel_commit(Some(persist_id.clone())).await?;
        debug!("discarding candidate changes");
        conn.discard_changes().await?;
        return Ok(());
    }

    match persist_id {
        Some(id) => {
            info!("Confirming commit persist-id: {id}");
            conn.confirm_commit(id.clone()).await?;
        }
        None => {
            debug!("committing candidate");
            conn.commit().await?;
        }
    }

    if !no_copy && conn.has_capability(STARTUP_CAP) {
        debug!("copying running to startup");
        conn.copy_config(CopySource::Running, Datastore::Startup)
            .await?;
    }
    Ok(())
}
