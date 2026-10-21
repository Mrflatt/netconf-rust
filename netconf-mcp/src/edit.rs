use std::str::FromStr;

use netconf_async::message::{
    CopySource, Datastore, DefaultOperation, EditContent, ErrorOption, TestOption,
};
use netconf_async::{
    CANDIDATE_CAP, CONFIRMED_COMMIT_10_CAP, CONFIRMED_COMMIT_CAP, Connection, NetconfClientError,
    NetconfClientResult, ROLLBACK_ON_ERROR_CAP, STARTUP_CAP, VALIDATE_10_CAP, VALIDATE_CAP,
    WRITABLE_RUNNING_CAP,
};
use uuid::Uuid;

use crate::types::EditConfigArgs;

/// Outcome of a completed edit. `warning` is set when unlock fails after
/// a successful commit so a persist-id is not discarded.
pub(crate) struct EditResult {
    pub persist_id: Option<String>,
    pub warning: Option<String>,
}

/// lock → edit-config → validate → commit → unlock → optional startup copy.
pub(crate) async fn execute_edit_config(
    conn: &mut Connection,
    args: &EditConfigArgs,
) -> NetconfClientResult<EditResult> {
    let has_candidate = conn.has_capability(CANDIDATE_CAP);
    let has_writable_running = conn.has_capability(WRITABLE_RUNNING_CAP);
    let has_rollback = conn.has_capability(ROLLBACK_ON_ERROR_CAP);
    let has_validate = conn.has_capability(VALIDATE_CAP) || conn.has_capability(VALIDATE_10_CAP);
    let has_startup = conn.has_capability(STARTUP_CAP);
    let has_confirmed =
        conn.has_capability(CONFIRMED_COMMIT_CAP) || conn.has_capability(CONFIRMED_COMMIT_10_CAP);

    let requested = args
        .target
        .as_deref()
        .map(Datastore::from_str)
        .transpose()?;
    let target = select_target(requested, has_candidate, has_writable_running)?;
    let default_operation = args
        .default_operation
        .as_deref()
        .map(DefaultOperation::from_str)
        .transpose()?;
    let test_option = args
        .test_option
        .as_deref()
        .map(TestOption::from_str)
        .transpose()?;
    let error_option = Some(select_error_option(None, has_rollback));
    let is_candidate = matches!(target, Datastore::Candidate);
    let persist_id = Uuid::new_v4().to_string();

    if args.confirmed && !is_candidate {
        return Err(NetconfClientError::new(
            "confirmed commit requires the candidate datastore",
        ));
    }
    if args.confirmed && !has_confirmed {
        return Err(NetconfClientError::new(
            "server does not support :confirmed-commit",
        ));
    }
    let commit_after_edit = should_commit(test_option, args.confirmed)?;
    let test_only = !commit_after_edit;
    copy_to_startup_ok(args.copy_to_startup, has_startup, test_only, args.confirmed)?;

    conn.lock(target.clone()).await?;

    let result: NetconfClientResult<Option<String>> = async {
        conn.edit_config(
            target.clone(),
            EditContent::Config(args.config.clone()),
            default_operation,
            test_option,
            error_option,
        )
        .await?;

        if has_validate && is_candidate && test_option.is_none() {
            conn.validate(target.clone()).await?;
        }

        let mut returned_persist = None;
        if is_candidate && commit_after_edit {
            if args.confirmed {
                conn.confirmed_commit(
                    confirm_timeout_secs(args.confirm_timeout)?,
                    Some(persist_id.clone()),
                    None,
                )
                .await?;
                returned_persist = Some(persist_id);
            } else {
                conn.commit().await?;
            }
        }

        if args.copy_to_startup {
            conn.copy_config(CopySource::Running, Datastore::Startup)
                .await?;
        }
        Ok(returned_persist)
    }
    .await;

    if result.is_err() && is_candidate {
        let _ = conn.discard_changes().await;
    }
    let unlock = conn.unlock(target).await;
    finish_edit(result, unlock)
}

pub(crate) fn select_target(
    requested: Option<Datastore>,
    has_candidate: bool,
    has_writable_running: bool,
) -> NetconfClientResult<Datastore> {
    match requested {
        Some(Datastore::Running) if !has_writable_running => Err(NetconfClientError::new(
            "writable-running:1.0 capability is not supported",
        )),
        Some(Datastore::Candidate) if !has_candidate => Err(NetconfClientError::new(
            "candidate:1.0 capability is not supported",
        )),
        Some(Datastore::Running) => Ok(Datastore::Running),
        Some(Datastore::Candidate) => Ok(Datastore::Candidate),
        Some(Datastore::Startup) => Err(NetconfClientError::new(
            "startup is not a valid edit-config target",
        )),
        Some(Datastore::Url(_)) => Err(NetconfClientError::new(
            "url is not a valid edit-config target",
        )),
        None if has_candidate => Ok(Datastore::Candidate),
        None if has_writable_running => Ok(Datastore::Running),
        None => Err(NetconfClientError::new(
            "neither :candidate nor :writable-running is advertised",
        )),
    }
}

pub(crate) fn should_commit(
    test_option: Option<TestOption>,
    confirmed: bool,
) -> NetconfClientResult<bool> {
    let test_only = matches!(test_option, Some(TestOption::TestOnly));
    if confirmed && test_only {
        return Err(NetconfClientError::new(
            "confirmed commit cannot be combined with test-only",
        ));
    }
    Ok(!test_only)
}

pub(crate) fn confirm_timeout_secs(timeout: Option<u32>) -> NetconfClientResult<Option<i32>> {
    timeout
        .map(i32::try_from)
        .transpose()
        .map_err(|_| NetconfClientError::new("confirm_timeout is too large"))
}

pub(crate) fn copy_to_startup_ok(
    requested: bool,
    has_startup: bool,
    test_only: bool,
    confirmed: bool,
) -> NetconfClientResult<()> {
    if !requested {
        return Ok(());
    }
    if !has_startup {
        return Err(NetconfClientError::new(
            "copy_to_startup requires the :startup capability",
        ));
    }
    if test_only {
        return Err(NetconfClientError::new(
            "copy_to_startup cannot be combined with test-only",
        ));
    }
    if confirmed {
        return Err(NetconfClientError::new(
            "copy_to_startup cannot be combined with confirmed commit",
        ));
    }
    Ok(())
}

pub(crate) fn finish_edit(
    result: NetconfClientResult<Option<String>>,
    unlock: NetconfClientResult<String>,
) -> NetconfClientResult<EditResult> {
    match (result, unlock) {
        (Err(err), _) => Err(err),
        (Ok(persist_id), Err(err)) => Ok(EditResult {
            persist_id,
            warning: Some(format!("unlock failed: {err}")),
        }),
        (Ok(persist_id), Ok(_)) => Ok(EditResult {
            persist_id,
            warning: None,
        }),
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
        assert!(select_target(Some(Datastore::Startup), true, true).is_err());
        assert!(select_target(Some(Datastore::Url("file:///x".into())), true, true).is_err());
    }

    #[test]
    fn copy_to_startup_rejects_missing_capability() {
        assert!(copy_to_startup_ok(false, false, false, false).is_ok());
        assert!(copy_to_startup_ok(true, true, false, false).is_ok());
        assert!(copy_to_startup_ok(true, false, false, false).is_err());
        assert!(copy_to_startup_ok(true, true, true, false).is_err());
        assert!(copy_to_startup_ok(true, true, false, true).is_err());
    }

    #[test]
    fn finish_edit_keeps_persist_id_when_unlock_fails() {
        let persist = Some("abc".to_string());
        let out = finish_edit(
            Ok(persist.clone()),
            Err(NetconfClientError::new("unlock boom")),
        )
        .unwrap();
        assert_eq!(out.persist_id, persist);
        assert!(out.warning.as_ref().is_some_and(|m| m.contains("unlock")));
        assert!(finish_edit(Err(NetconfClientError::new("edit boom")), Ok(String::new())).is_err());
    }

    #[test]
    fn confirm_timeout_rejects_overflow() {
        assert_eq!(confirm_timeout_secs(Some(60)).unwrap(), Some(60));
        assert!(confirm_timeout_secs(Some(u32::MAX)).is_err());
    }

    #[test]
    fn test_only_cannot_be_confirmed() {
        assert!(should_commit(Some(TestOption::TestOnly), false).is_ok_and(|c| !c));
        assert!(should_commit(Some(TestOption::TestOnly), true).is_err());
        assert!(should_commit(None, false).is_ok_and(|c| c));
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
    }
}
