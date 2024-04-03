use crate::commands::*;
use crate::config::Config;
use clap::builder::{IntoResettable, ValueParser};
use clap::{Arg, ArgMatches, Command, ValueHint};
use netconf_async::connection::Connection;
use netconf_async::error::NetconfClientResult;

pub fn builtin() -> Vec<Command> {
    vec![get::cli(), get_config::cli(), notification::cli()]
}

pub async fn builtin_exec(
    cmd: &str,
    conn: &mut Connection,
    args: &Config,
) -> Option<NetconfClientResult<()>> {
    let f = match cmd {
        "get" => get::exec(args, conn).await,
        "get-config" => get_config::exec(args, conn).await,
        "notification" => notification::exec(args, conn).await,
        _ => return None,
    };
    Some(f)
}

pub(crate) fn value_of<'a, T: Clone + Send + Sync + 'static>(
    name: &str,
    args: &'a ArgMatches,
) -> &'a T {
    args.get_one::<T>(name).unwrap()
}

pub(crate) fn value_of_if_exists<'a, T: Clone + Send + Sync + 'static>(
    name: &str,
    args: &'a ArgMatches,
) -> Option<&'a T> {
    if args.contains_id(name) {
        args.get_one::<T>(name)
    } else {
        None
    }
}

pub(crate) fn values_of<'a, T: Clone + Send + Sync + 'static>(
    name: &str,
    args: &'a ArgMatches,
) -> Vec<&'a T> {
    args.get_many::<T>(name).unwrap_or_default().collect()
}

pub(super) fn arg(
    name: &'static str,
    help: &'static str,
    required: bool,
    short: Option<char>,
    default: Option<&'static str>,
    hint: Option<ValueHint>,
    parser: impl IntoResettable<ValueParser>,
) -> Arg {
    Arg::new(name)
        .short(short)
        .long(name)
        .help(help)
        .required(required)
        .default_value(default)
        .value_hint(hint)
        .value_parser(parser)
}
