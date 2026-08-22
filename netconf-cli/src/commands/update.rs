use crate::commands::builtin::value_of;
use crate::update::Updater;
use clap::{Arg, ArgAction, ArgMatches, Command};
use log::info;
use netconf_async::error::NetconfClientResult;

pub fn cli() -> Command {
    Command::new("update")
        .about("Update netconf-cli from GitHub releases")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            Arg::new("check")
                .long("check")
                .action(ArgAction::SetTrue)
                .help("Only check for a new release"),
            Arg::new("force")
                .long("force")
                .action(ArgAction::SetTrue)
                .help("Reinstall the latest release even if already up to date"),
        ])
}

pub async fn exec(args: &ArgMatches) -> NetconfClientResult<()> {
    let check = *value_of::<bool>("check", args);
    let force = *value_of::<bool>("force", args);

    let updater = Updater::from_env()?;
    let polled = updater.poll().await?;

    if polled.update_available() {
        info!(
            "netconf-cli {} available (current {})",
            polled.latest.version, polled.current
        );
        if check {
            return Ok(());
        }
        updater.apply(&polled.latest).await?;
        info!("Updated to {}", polled.latest.version);
        return Ok(());
    }

    info!(
        "netconf-cli {} is up to date (latest {})",
        polled.current, polled.latest.version
    );
    if check || !force {
        return Ok(());
    }
    updater.apply(&polled.latest).await?;
    info!("Reinstalled {}", polled.latest.version);
    Ok(())
}
