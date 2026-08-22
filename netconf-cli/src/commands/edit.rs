use crate::commands::builtin::{arg, value_of, value_of_if_exists, xml_inputs_from_args};
use crate::config::Config;
use clap::{Arg, ArgAction, Command, ValueHint, arg};
use log::{debug, error, info, warn};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::message::{
    CopySource, Datastore, DefaultOperation, EditContent, ErrorOption, TestOption,
};
use netconf_async::{
    CANDIDATE_CAP, CONFIRMED_COMMIT_10_CAP, CONFIRMED_COMMIT_CAP, ROLLBACK_ON_ERROR_CAP,
    STARTUP_CAP, VALIDATE_10_CAP, VALIDATE_CAP, WRITABLE_RUNNING_CAP,
};
use std::str::FromStr;
use uuid::Uuid;

pub fn cli() -> Command {
    Command::new("edit")
        .about("Execute edit-config rpc")
        .help_template(color_print::cstr!(
            "\
{about-with-newline}
<green,bold>Usage:</> {usage}

<green,bold>Options:</>
{options}\n",
        ))
        .args([
            arg(
                "target",
                "Datastore to edit (default: candidate if advertised, else running)",
                false,
                Some('t'),
                None,
                None,
                ["running", "candidate"],
            ),
            arg(
                "file",
                "Config XML, file or directory, @path, or '-' for stdin (name order, committed once)",
                false,
                Some('f'),
                None,
                Some(ValueHint::AnyPath),
                None,
            )
            .required_unless_present("url")
            .conflicts_with("url"),
            arg(
                "url",
                "URL of the config to apply (:url capability)",
                false,
                None,
                None,
                None,
                None,
            ),
            arg(
                "default-operation",
                "Default merge strategy",
                false,
                None,
                Some("merge"),
                None,
                ["merge", "replace", "none"],
            ),
            arg(
                "test-option",
                "Validate before applying (:validate)",
                false,
                None,
                None,
                None,
                ["test-then-set", "set", "test-only"],
            ),
            arg(
                "error-option",
                "Behavior when an error is encountered (default: rollback-on-error if advertised)",
                false,
                None,
                None,
                None,
                ["stop-on-error", "continue-on-error", "rollback-on-error"],
            ),
            arg!(--confirmed "Use a persist confirmed-commit; print persist-id for `commit`"),
            arg(
                "persist-id",
                "Persist id for confirmed commit (default: random uuid)",
                false,
                None,
                None,
                None,
                None,
            ),
            arg(
                "commit-timeout",
                "Confirmed commit timeout in seconds",
                false,
                None,
                Some("300"),
                None,
                None,
            ),
            arg!(--"skip-lock" "Do not lock/unlock the target datastore"),
            Arg::new("no-copy")
                .long("no-copy")
                .help("Do not copy running to startup after commit")
                .action(ArgAction::SetTrue),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    match run(cfg, conn).await {
        Ok(()) => Ok(()),
        Err(err) => {
            error!("Edit error: {}", err);
            Err(err)
        }
    }
}

async fn run(cfg: &Config, conn: &mut Connection) -> NetconfClientResult<()> {
    let has_candidate = conn.has_capability(CANDIDATE_CAP);
    let has_writable_running = conn.has_capability(WRITABLE_RUNNING_CAP);
    let has_rollback = conn.has_capability(ROLLBACK_ON_ERROR_CAP);
    let has_validate = conn.has_capability(VALIDATE_CAP) || conn.has_capability(VALIDATE_10_CAP);
    let has_startup = conn.has_capability(STARTUP_CAP);
    let has_confirmed =
        conn.has_capability(CONFIRMED_COMMIT_CAP) || conn.has_capability(CONFIRMED_COMMIT_10_CAP);

    let requested = value_of_if_exists::<String>("target", &cfg.args)
        .map(|value| Datastore::from_str(value))
        .transpose()?;
    let target = select_target(requested, has_candidate, has_writable_running)?;

    let url = value_of_if_exists::<String>("url", &cfg.args).cloned();
    let files = if url.is_some() {
        Vec::new()
    } else {
        let files = xml_inputs_from_args("file", cfg)?;
        if files.is_empty() {
            return Err(NetconfClientError::new(
                "config file/directory or url required".to_string(),
            ));
        }
        files
    };

    let default_operation = value_of_if_exists::<String>("default-operation", &cfg.args)
        .map(|value| DefaultOperation::from_str(value))
        .transpose()?;
    let test_option = value_of_if_exists::<String>("test-option", &cfg.args)
        .map(|value| TestOption::from_str(value))
        .transpose()?;
    let requested_error = value_of_if_exists::<String>("error-option", &cfg.args)
        .map(|value| ErrorOption::from_str(value))
        .transpose()?;
    let error_option = Some(select_error_option(requested_error, has_rollback));

    let confirmed = *value_of::<bool>("confirmed", &cfg.args);
    let skip_lock = *value_of::<bool>("skip-lock", &cfg.args);
    let no_copy = *value_of::<bool>("no-copy", &cfg.args);
    let persist_id = value_of_if_exists::<String>("persist-id", &cfg.args)
        .cloned()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let commit_timeout = value_of::<String>("commit-timeout", &cfg.args)
        .parse::<i32>()
        .map_err(|err| NetconfClientError::new(format!("invalid commit-timeout: {err}")))?;

    let is_candidate = matches!(target, Datastore::Candidate);
    if confirmed && !is_candidate {
        return Err(NetconfClientError::new(
            "confirmed commit requires the candidate datastore".to_string(),
        ));
    }
    if confirmed && !has_confirmed {
        return Err(NetconfClientError::new(
            "server does not support :confirmed-commit".to_string(),
        ));
    }
    if !skip_lock {
        debug!("Locking {target:?} datastore");
        conn.lock(target.clone()).await?;
    }

    let result: NetconfClientResult<()> = async {
        if let Some(url) = url {
            conn.edit_config(
                target.clone(),
                EditContent::Url(url),
                default_operation,
                test_option,
                error_option,
            )
            .await?;
        } else {
            for file in &files {
                debug!("Applying {}", file.name);
                conn.edit_config(
                    target.clone(),
                    EditContent::Config(file.content.clone()),
                    default_operation,
                    test_option,
                    error_option,
                )
                .await?;
            }
        }

        if has_validate && is_candidate && test_option.is_none() {
            debug!("Validating candidate datastore");
            conn.validate(target.clone()).await?;
        }

        if is_candidate {
            debug!("Committing candidate");
            if confirmed {
                conn.confirmed_commit(Some(commit_timeout), Some(persist_id.clone()), None)
                    .await?;
            } else {
                conn.commit().await?;
            }
        }
        Ok(())
    }
    .await;

    if result.is_err() && is_candidate {
        debug!("Discarding candidate changes after error");
        if let Err(discard_err) = conn.discard_changes().await {
            warn!("Discard-changes failed: {discard_err}");
        }
    }

    if !skip_lock && let Err(unlock_err) = conn.unlock(target.clone()).await {
        warn!("Unlock failed: {unlock_err}");
        result?;
        return Err(unlock_err);
    }
    result?;

    if confirmed {
        info!("Confirmed commit persist-id: {persist_id}");
    }

    let test_only = matches!(test_option, Some(TestOption::TestOnly));
    if !no_copy && has_startup && !test_only && !confirmed {
        debug!("Copying running to startup");
        conn.copy_config(CopySource::Running, Datastore::Startup)
            .await?;
    }
    Ok(())
}

fn select_target(
    requested: Option<Datastore>,
    has_candidate: bool,
    has_writable_running: bool,
) -> NetconfClientResult<Datastore> {
    match requested {
        Some(Datastore::Running) if !has_writable_running => Err(NetconfClientError::new(
            "writable-running:1.0 capability is not supported".to_string(),
        )),
        Some(Datastore::Candidate) if !has_candidate => Err(NetconfClientError::new(
            "candidate:1.0 capability is not supported".to_string(),
        )),
        Some(datastore) => Ok(datastore),
        None if has_candidate => Ok(Datastore::Candidate),
        None if has_writable_running => Ok(Datastore::Running),
        None => Err(NetconfClientError::new(
            "neither :candidate nor :writable-running is advertised".to_string(),
        )),
    }
}

fn select_error_option(requested: Option<ErrorOption>, has_rollback: bool) -> ErrorOption {
    match requested {
        Some(option) => option,
        None if has_rollback => ErrorOption::RollbackOnError,
        None => ErrorOption::StopOnError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_target_prefers_candidate() {
        assert!(matches!(
            select_target(None, true, true).unwrap(),
            Datastore::Candidate
        ));
        assert!(matches!(
            select_target(None, false, true).unwrap(),
            Datastore::Running
        ));
        assert!(select_target(None, false, false).is_err());
        assert!(select_target(Some(Datastore::Running), true, false).is_err());
        assert!(select_target(Some(Datastore::Candidate), false, true).is_err());
    }

    #[test]
    fn select_error_option_uses_rollback_when_advertised() {
        assert!(matches!(
            select_error_option(None, true),
            ErrorOption::RollbackOnError
        ));
        assert!(matches!(
            select_error_option(None, false),
            ErrorOption::StopOnError
        ));
        assert!(matches!(
            select_error_option(Some(ErrorOption::ContinueOnError), true),
            ErrorOption::ContinueOnError
        ));
    }
}
