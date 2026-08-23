use crate::commands::builtin::{arg, filter_from_args, value_of, value_of_if_exists};
use crate::config::Config;
use clap::{Command, ValueHint, arg};
use log::{error, info};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::message::{self, Filter};
use std::time::Duration;
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
                "Subtree filter XML, file path, @file, or '-' for stdin",
                false,
                Some('f'),
                None,
                Some(ValueHint::FilePath),
                None,
            ),
            arg(
                "start-time",
                "Replay startTime (RFC3339, e.g. 2026-01-01T00:00:00Z)",
                false,
                None,
                None,
                None,
                None,
            )
            .conflicts_with("get"),
            arg(
                "stop-time",
                "Replay stopTime (RFC3339; requires --start-time)",
                false,
                None,
                None,
                None,
                None,
            )
            .requires("start-time")
            .conflicts_with("get"),
            arg(
                "duration",
                "Replay/listen window (15s, 5m, 1h, or seconds). Sets stopTime; startTime defaults to now",
                false,
                None,
                None,
                None,
                None,
            )
            .conflicts_with("stop-time")
            .conflicts_with("get"),
            arg!(-g --get "Get available notification streams").global(true),
        ])
}

pub async fn exec(cfg: &Config, conn: &mut Connection, host: &str) -> NetconfClientResult<()> {
    let args = &cfg.args;
    let get_streams = value_of::<bool>("get", args);
    if *get_streams {
        let filter = Filter::subtree(
            r#"<netconf xmlns="urn:ietf:params:xml:ns:netmod:notification"><streams/></netconf>"#,
        );
        let resp = conn.get(Some(filter), None).await?;
        cfg.output.emit(host, &resp)
    } else {
        let stream = value_of::<String>("stream", args);
        let filter = filter_from_args(cfg)?;
        let start_time = value_of_if_exists::<String>("start-time", args).map(String::as_str);
        let stop_time = value_of_if_exists::<String>("stop-time", args).map(String::as_str);
        let duration = value_of_if_exists::<String>("duration", args)
            .map(|value| parse_duration_arg(value))
            .transpose()?;
        let replay = duration
            .map(|duration| message::replay_window(start_time, duration))
            .transpose()?;
        let (start_time, stop_time) = match &replay {
            Some((start, stop)) => (Some(start.as_str()), Some(stop.as_str())),
            None => (start_time, stop_time),
        };
        let (tx, mut rx) = channel::<String>(1);
        let output = cfg.output.clone();
        let host = host.to_string();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(err) = output.emit(&host, &msg) {
                    error!("Failed to write notification: {err}");
                }
            }
        });
        // The library streams until the device stops or the future is dropped,
        // so Ctrl-C is handled here rather than inside the session.
        tokio::select! {
            result = conn.notification(tx, Some(stream), filter, start_time, stop_time) => result,
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(NetconfClientError::Io)?;
                info!("Stopping notification listener");
                Ok(())
            }
        }
    }
}

fn parse_duration_arg(value: &str) -> NetconfClientResult<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return Err(NetconfClientError::new("empty duration"));
    }
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split);
    if digits.is_empty() {
        return Err(NetconfClientError::new(format!(
            "invalid duration '{value}'"
        )));
    }
    let n: u64 = digits
        .parse()
        .map_err(|err| NetconfClientError::new(format!("invalid duration '{value}': {err}")))?;
    if n == 0 {
        return Err(NetconfClientError::new(
            "duration must be greater than zero",
        ));
    }
    let secs = match unit {
        "" | "s" | "S" => n,
        "m" | "M" => n
            .checked_mul(60)
            .ok_or_else(|| NetconfClientError::new(format!("duration too large: '{value}'")))?,
        "h" | "H" => n
            .checked_mul(3600)
            .ok_or_else(|| NetconfClientError::new(format!("duration too large: '{value}'")))?,
        _ => {
            return Err(NetconfClientError::new(format!(
                "invalid duration '{value}' (use 15s, 5m, 1h, or seconds)"
            )));
        }
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_time_requires_start_time() {
        let err = cli()
            .try_get_matches_from(["notification", "--stop-time", "2026-01-01T00:01:00Z"])
            .unwrap_err();
        assert!(err.to_string().contains("start-time"), "{err}");
    }

    #[test]
    fn duration_conflicts_with_stop_time() {
        let err = cli()
            .try_get_matches_from([
                "notification",
                "--start-time",
                "2026-01-01T00:00:00Z",
                "--stop-time",
                "2026-01-01T00:01:00Z",
                "--duration",
                "5m",
            ])
            .unwrap_err();
        assert!(err.to_string().contains("duration"), "{err}");
    }

    #[test]
    fn parse_duration_arg_units() {
        assert_eq!(parse_duration_arg("15s").unwrap(), Duration::from_secs(15));
        assert_eq!(parse_duration_arg("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration_arg("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration_arg("30").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration_arg("2H").unwrap(), Duration::from_secs(7200));
        assert!(parse_duration_arg("0").is_err());
        assert!(parse_duration_arg("5x").is_err());
        assert!(parse_duration_arg("").is_err());
        assert!(parse_duration_arg("m").is_err());
    }
}
