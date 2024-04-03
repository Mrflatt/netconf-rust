use crate::commands::builtin::{arg, value_of};
use crate::config::Config;
use clap::{arg, Command, ValueHint};
use log::{error, info};
use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientResult;
use netconf_async::message::Filter;
use tokio::sync::mpsc::channel;

pub fn cli() -> Command {
    Command::new("notification")
        .about("Execute create-subscription rpc")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            arg(
                "stream",
                "Stream to subscribe",
                false,
                Some('s'),
                Some("NETCONF"),
                None,
                None,
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
            arg!(-g --get "Get available notification streams").global(true),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let args = &cfg.args;
    let get_streams = value_of::<bool>("get", args);
    if *get_streams {
        let filter = Filter::subtree(
            r#"<netconf xmlns="urn:ietf:params:xml:ns:netmod:notification"><streams/></netconf>"#,
        );
        match conn.get(Some(filter), None).await {
            Ok(resp) => {
                info!("Available notification streams:\n{}", resp);
            }
            Err(err) => {
                error!("Get error: {}", err);
            }
        };
        Ok(())
    } else {
        let stream = value_of::<String>("stream", args);
        let (tx, mut rx) = channel::<String>(1);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                info!("Notification:\n{}", msg);
            }
        });
        conn.notification(tx, Some(stream), None).await
    }
}
