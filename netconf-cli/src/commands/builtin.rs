use crate::commands::*;
use crate::config::Config;
use clap::builder::{IntoResettable, ValueParser};
use clap::{Arg, ArgMatches, Command, ValueHint};
use netconf_async::connection::Connection;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use netconf_async::message::Filter;
use std::path::Path;

pub fn builtin() -> Vec<Command> {
    vec![
        get::cli(),
        get_config::cli(),
        edit::cli(),
        copy::cli(),
        commit::cli(),
        rpc::cli(),
        notification::cli(),
    ]
}

pub async fn builtin_exec(
    cmd: &str,
    conn: &mut Connection,
    args: &Config,
    host: &str,
) -> Option<NetconfClientResult<()>> {
    let f = match cmd {
        "get" => get::exec(args, conn, host).await,
        "get-config" => get_config::exec(args, conn, host).await,
        "edit" => edit::exec(args, conn).await,
        "copy" => copy::exec(args, conn, host).await,
        "commit" => commit::exec(args, conn).await,
        "rpc" => rpc::exec(args, conn, host).await,
        "notification" => notification::exec(args, conn, host).await,
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

pub(crate) struct XmlInput {
    pub name: String,
    pub content: String,
}

pub(crate) fn xml_file_from_args(
    name: &str,
    args: &ArgMatches,
) -> NetconfClientResult<Option<String>> {
    match value_of_if_exists::<String>(name, args) {
        Some(path) => {
            let content = std::fs::read_to_string(path).map_err(|err| {
                NetconfClientError::new(format!("failed to read {name} '{path}': {err}"))
            })?;
            Ok(Some(content))
        }
        None => Ok(None),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum XmlArgSource<'a> {
    Inline(&'a str),
    File(&'a str),
    Stdin,
}

fn xml_arg_source(value: &str) -> XmlArgSource<'_> {
    if value == "-" {
        XmlArgSource::Stdin
    } else if let Some(path) = value.strip_prefix('@') {
        XmlArgSource::File(path)
    } else if value.trim_start().starts_with('<') {
        XmlArgSource::Inline(value)
    } else {
        XmlArgSource::File(value)
    }
}

fn read_xml_arg(name: &str, value: &str, stdin: Option<&str>) -> NetconfClientResult<String> {
    match xml_arg_source(value) {
        XmlArgSource::Inline(xml) => Ok(xml.to_string()),
        XmlArgSource::Stdin => stdin
            .map(str::to_string)
            .ok_or_else(|| NetconfClientError::new(format!("failed to read {name} from stdin"))),
        XmlArgSource::File(path) => std::fs::read_to_string(path).map_err(|err| {
            NetconfClientError::new(format!("failed to read {name} '{path}': {err}"))
        }),
    }
}

pub(crate) fn filter_from_args(cfg: &Config) -> NetconfClientResult<Option<Filter>> {
    match value_of_if_exists::<String>("filter", &cfg.args) {
        Some(value) => Ok(Some(Filter::subtree(&read_xml_arg(
            "filter",
            value,
            cfg.stdin_xml.as_deref(),
        )?))),
        None => Ok(None),
    }
}

pub(crate) fn xml_inputs_from_args(name: &str, cfg: &Config) -> NetconfClientResult<Vec<XmlInput>> {
    match value_of_if_exists::<String>(name, &cfg.args) {
        Some(value) => read_xml_arg_inputs(name, value, cfg.stdin_xml.as_deref()),
        None => Ok(Vec::new()),
    }
}

fn read_xml_arg_inputs(
    name: &str,
    value: &str,
    stdin: Option<&str>,
) -> NetconfClientResult<Vec<XmlInput>> {
    match xml_arg_source(value) {
        XmlArgSource::Inline(xml) => Ok(vec![XmlInput {
            name: "<inline>".to_string(),
            content: xml.to_string(),
        }]),
        XmlArgSource::Stdin => {
            let content = read_xml_arg(name, "-", stdin)?;
            Ok(vec![XmlInput {
                name: "-".to_string(),
                content,
            }])
        }
        XmlArgSource::File(path) => read_xml_inputs(path),
    }
}

pub(crate) fn read_xml_inputs(path: &str) -> NetconfClientResult<Vec<XmlInput>> {
    let meta = std::fs::metadata(path)
        .map_err(|err| NetconfClientError::new(format!("failed to read '{path}': {err}")))?;
    if meta.is_dir() {
        read_xml_dir(path)
    } else {
        Ok(vec![read_xml_file(Path::new(path))?])
    }
}

fn read_xml_file(path: &Path) -> NetconfClientResult<XmlInput> {
    let display = path.display();
    let content = std::fs::read_to_string(path)
        .map_err(|err| NetconfClientError::new(format!("failed to read '{display}': {err}")))?;
    Ok(XmlInput {
        name: display.to_string(),
        content,
    })
}

fn is_xml_filename(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
}

fn read_xml_dir(path: &str) -> NetconfClientResult<Vec<XmlInput>> {
    let mut files = Vec::new();
    let dir = std::fs::read_dir(path).map_err(|err| {
        NetconfClientError::new(format!("failed to read directory '{path}': {err}"))
    })?;
    for entry in dir {
        let entry = entry.map_err(|err| {
            NetconfClientError::new(format!("failed to read directory '{path}': {err}"))
        })?;
        let file_type = entry.file_type().map_err(|err| {
            NetconfClientError::new(format!("failed to read directory '{path}': {err}"))
        })?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        if !is_xml_filename(&name.to_string_lossy()) {
            continue;
        }
        files.push(entry.path());
    }
    files.sort();
    if files.is_empty() {
        return Err(NetconfClientError::new(format!(
            "no .xml files in directory '{path}'"
        )));
    }
    files.iter().map(|file| read_xml_file(file)).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("netconf-cli-xml-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_xml_dir_sorts_by_name_and_skips_non_xml() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("2-rpc.xml"), "<b/>").unwrap();
        std::fs::write(dir.path().join("1-rpc.xml"), "<a/>").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "nope").unwrap();
        std::fs::write(dir.path().join(".hidden.xml"), "<h/>").unwrap();

        let inputs = read_xml_inputs(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(inputs.len(), 2);
        assert!(inputs[0].name.ends_with("1-rpc.xml"));
        assert_eq!(inputs[0].content, "<a/>");
        assert!(inputs[1].name.ends_with("2-rpc.xml"));
        assert_eq!(inputs[1].content, "<b/>");
    }

    #[test]
    fn read_xml_file_returns_single_input() {
        let dir = TempDir::new();
        let file = dir.path().join("only.xml");
        std::fs::write(&file, "<rpc/>").unwrap();

        let inputs = read_xml_inputs(file.to_str().unwrap()).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].content, "<rpc/>");
    }

    #[test]
    fn read_xml_dir_errors_when_empty() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("readme.txt"), "no xml").unwrap();
        assert!(read_xml_inputs(dir.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn xml_arg_source_inline_file_at_and_stdin() {
        assert_eq!(
            xml_arg_source("<configure/>"),
            XmlArgSource::Inline("<configure/>")
        );
        assert_eq!(
            xml_arg_source("  </configure>"),
            XmlArgSource::Inline("  </configure>")
        );
        assert_eq!(xml_arg_source("-"), XmlArgSource::Stdin);
        assert_eq!(
            xml_arg_source("filter.xml"),
            XmlArgSource::File("filter.xml")
        );
        assert_eq!(
            xml_arg_source("@filter.xml"),
            XmlArgSource::File("filter.xml")
        );
        assert_eq!(xml_arg_source("@-"), XmlArgSource::File("-"));
        assert_eq!(xml_arg_source("./<weird>"), XmlArgSource::File("./<weird>"));
    }

    #[test]
    fn read_xml_arg_inline_and_file() {
        let dir = TempDir::new();
        let file = dir.path().join("filter.xml");
        std::fs::write(&file, "<top/>").unwrap();
        let path = file.to_str().unwrap();

        assert_eq!(
            read_xml_arg("filter", "<configure/>", None).unwrap(),
            "<configure/>"
        );
        assert_eq!(read_xml_arg("filter", path, None).unwrap(), "<top/>");
        assert_eq!(
            read_xml_arg("filter", &format!("@{path}"), None).unwrap(),
            "<top/>"
        );
        assert_eq!(
            read_xml_arg("filter", "-", Some("<from-stdin/>")).unwrap(),
            "<from-stdin/>"
        );
        let err = read_xml_arg("filter", "missing.xml", None).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to read filter 'missing.xml'"),
            "{err}"
        );
    }

    #[test]
    fn read_xml_arg_inputs_inline_file_and_stdin() {
        let dir = TempDir::new();
        let file = dir.path().join("edit.xml");
        std::fs::write(&file, "<config/>").unwrap();
        let path = file.to_str().unwrap();

        let inline = read_xml_arg_inputs("file", "<config/>", None).unwrap();
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].name, "<inline>");
        assert_eq!(inline[0].content, "<config/>");

        let from_file = read_xml_arg_inputs("file", path, None).unwrap();
        assert_eq!(from_file[0].content, "<config/>");

        let from_at = read_xml_arg_inputs("file", &format!("@{path}"), None).unwrap();
        assert_eq!(from_at[0].content, "<config/>");

        let from_stdin = read_xml_arg_inputs("file", "-", Some("<piped/>")).unwrap();
        assert_eq!(from_stdin[0].name, "-");
        assert_eq!(from_stdin[0].content, "<piped/>");
    }
}
