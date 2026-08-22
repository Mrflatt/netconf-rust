use netconf_async::error::{NetconfClientError, NetconfClientResult};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// How a reply is encoded and optionally rewritten before emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub kind: FormatKind,
    pub pretty: bool,
    pub unescape: bool,
}

/// Reply codec. New variants can be added without changing the flag shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Xml,
    Json,
}

impl Default for Format {
    fn default() -> Self {
        Self {
            kind: FormatKind::Xml,
            pretty: false,
            unescape: false,
        }
    }
}

impl Format {
    pub fn extension(&self) -> &'static str {
        match self.kind {
            FormatKind::Xml => "xml",
            FormatKind::Json => "json",
        }
    }

    fn render(&self, xml: &str) -> NetconfClientResult<String> {
        let prepared = if self.unescape {
            unescape_xml(xml)?
        } else {
            xml.to_string()
        };
        match self.kind {
            FormatKind::Xml if self.pretty => pretty_xml(&prepared),
            FormatKind::Xml => Ok(prepared),
            FormatKind::Json => xml_to_json_string(&prepared, self.pretty),
        }
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_format(value)
    }
}

/// Comma-separated tokens: `xml` (default) or `json`, plus `pretty` and `unescape`.
pub fn parse_format(value: &str) -> Result<Format, String> {
    let mut format = Format::default();
    let mut saw_kind = false;
    for raw in value.split(',') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        match token {
            "xml" => {
                if saw_kind && format.kind != FormatKind::Xml {
                    return Err("format cannot mix xml and json".to_string());
                }
                format.kind = FormatKind::Xml;
                saw_kind = true;
            }
            "json" => {
                if saw_kind && format.kind != FormatKind::Json {
                    return Err("format cannot mix xml and json".to_string());
                }
                format.kind = FormatKind::Json;
                saw_kind = true;
            }
            "pretty" => format.pretty = true,
            "unescape" => format.unescape = true,
            other => {
                return Err(format!(
                    "unknown format option '{other}' (use xml, json, pretty, unescape)"
                ));
            }
        }
    }
    Ok(format)
}

/// Writes formatted replies to stdout, or to `DIR/{host}.{ext}` when a directory is set.
#[derive(Clone, Debug)]
pub struct Output {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    format: Format,
    dir: Option<PathBuf>,
    started: Mutex<HashSet<PathBuf>>,
}

impl Output {
    pub fn new(format: Format, dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                format,
                dir,
                started: Mutex::new(HashSet::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn format_for_test(&self) -> &Format {
        &self.inner.format
    }

    #[cfg(test)]
    pub(crate) fn dir_for_test(&self) -> Option<&Path> {
        self.inner.dir.as_deref()
    }

    pub fn emit(&self, host: &str, xml: &str) -> NetconfClientResult<()> {
        let mut body = self.inner.format.render(xml)?;
        if !body.ends_with('\n') {
            body.push('\n');
        }
        match &self.inner.dir {
            Some(dir) => write_host_file(
                dir,
                host,
                self.inner.format.extension(),
                &body,
                &self.inner.started,
            ),
            None => write_stdout(&body),
        }
    }
}

fn write_stdout(body: &str) -> NetconfClientResult<()> {
    let mut out = io::stdout().lock();
    out.write_all(body.as_bytes())
        .map_err(|err| NetconfClientError::new(format!("failed to write reply: {err}")))?;
    out.flush()
        .map_err(|err| NetconfClientError::new(format!("failed to write reply: {err}")))
}

fn write_host_file(
    dir: &Path,
    host: &str,
    ext: &str,
    body: &str,
    started: &Mutex<HashSet<PathBuf>>,
) -> NetconfClientResult<()> {
    create_dir_all(dir).map_err(|err| {
        NetconfClientError::new(format!(
            "failed to create output directory '{}': {err}",
            dir.display()
        ))
    })?;
    let path = dir.join(host_filename(host, ext));
    let first = {
        let mut seen = started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        seen.insert(path.clone())
    };
    let mut file = open_host_file(&path, first)?;
    file.write_all(body.as_bytes()).map_err(|err| {
        NetconfClientError::new(format!("failed to write '{}': {err}", path.display()))
    })
}

fn open_host_file(path: &Path, first: bool) -> NetconfClientResult<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(first)
        .append(!first)
        .open(path)
        .map_err(|err| {
            NetconfClientError::new(format!("failed to open '{}': {err}", path.display()))
        })
}

fn host_filename(host: &str, ext: &str) -> String {
    let trimmed = host.trim().trim_start_matches('[').replace(']', "");
    let mut name = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            name.push(c);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        name.push_str("host");
    }
    format!("{name}.{ext}")
}

fn unescape_xml(xml: &str) -> NetconfClientResult<String> {
    quick_xml::escape::unescape(xml)
        .map(|cow| cow.into_owned())
        .map_err(|err| NetconfClientError::new(format!("failed to unescape xml: {err}")))
}

fn pretty_xml(xml: &str) -> NetconfClientResult<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(event) => writer.write_event(event).map_err(|err| {
                NetconfClientError::new(format!("failed to pretty-print xml: {err}"))
            })?,
            Err(err) => {
                return Err(NetconfClientError::new(format!(
                    "failed to pretty-print xml: {err}"
                )));
            }
        }
        buf.clear();
    }
    let bytes = writer.into_inner().into_inner();
    String::from_utf8(bytes)
        .map_err(|err| NetconfClientError::new(format!("failed to pretty-print xml: {err}")))
}

fn xml_to_json_string(xml: &str, pretty: bool) -> NetconfClientResult<String> {
    let value = xml_to_json(xml)?;
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|err| NetconfClientError::new(format!("failed to encode json: {err}")))
}

fn xml_to_json(xml: &str) -> NetconfClientResult<Value> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack = vec![Builder::root()];
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(start)) => stack.push(Builder::from_start(&start)?),
            Ok(Event::Empty(start)) => {
                let child = Builder::from_start(&start)?.into_value();
                current(&mut stack)?.push_child(child);
            }
            Ok(Event::Text(text)) => {
                current(&mut stack)?.push_text(&text.decode().map_err(xml_error)?);
            }
            Ok(Event::CData(text)) => {
                current(&mut stack)?.push_text(&text.decode().map_err(xml_error)?);
            }
            Ok(Event::End(_)) => {
                let finished = stack.pop().ok_or_else(|| {
                    NetconfClientError::new("unbalanced xml while converting to json")
                })?;
                current(&mut stack)?.push_child(finished.into_value());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => return Err(xml_error(err)),
        }
        buf.clear();
    }
    if stack.len() != 1 {
        return Err(NetconfClientError::new(
            "unbalanced xml while converting to json",
        ));
    }
    Ok(stack.pop().expect("root").into_document())
}

fn current(stack: &mut [Builder]) -> NetconfClientResult<&mut Builder> {
    stack
        .last_mut()
        .ok_or_else(|| NetconfClientError::new("unbalanced xml while converting to json"))
}

fn xml_error(err: impl std::fmt::Display) -> NetconfClientError {
    NetconfClientError::new(format!("failed to convert xml to json: {err}"))
}

struct Builder {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<(String, Value)>,
}

impl Builder {
    fn root() -> Self {
        Self {
            name: String::new(),
            attrs: Vec::new(),
            text: String::new(),
            children: Vec::new(),
        }
    }

    fn from_start(start: &BytesStart<'_>) -> NetconfClientResult<Self> {
        let name = qname_to_string(start.name().as_ref())?;
        let mut attrs = Vec::new();
        for attr in start.attributes() {
            let attr = attr.map_err(xml_error)?;
            let key = qname_to_string(attr.key.as_ref())?;
            let raw = std::str::from_utf8(attr.value.as_ref()).map_err(xml_error)?;
            let value = unescape_xml(raw)?;
            attrs.push((format!("@{key}"), value));
        }
        Ok(Self {
            name,
            attrs,
            text: String::new(),
            children: Vec::new(),
        })
    }

    fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn push_child(&mut self, child: (String, Value)) {
        self.children.push(child);
    }

    fn into_value(self) -> (String, Value) {
        let name = self.name.clone();
        (name, self.build_value())
    }

    fn into_document(self) -> Value {
        match self.children.len() {
            0 if self.text.is_empty() => Value::Null,
            0 => Value::String(self.text),
            1 if self.attrs.is_empty() && self.text.is_empty() => {
                let (name, value) = self.children.into_iter().next().expect("one child");
                Value::Object(Map::from_iter([(name, value)]))
            }
            _ => object_from_parts(self.attrs, self.text, self.children),
        }
    }

    fn build_value(self) -> Value {
        let has_attrs = !self.attrs.is_empty();
        let has_children = !self.children.is_empty();
        let has_text = !self.text.is_empty();
        match (has_attrs, has_children, has_text) {
            (false, false, false) => Value::Object(Map::new()),
            (false, false, true) => Value::String(self.text),
            _ => object_from_parts(self.attrs, self.text, self.children),
        }
    }
}

fn object_from_parts(
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<(String, Value)>,
) -> Value {
    let mut map = Map::new();
    for (key, value) in attrs {
        map.insert(key, Value::String(value));
    }
    if !text.is_empty() {
        map.insert("#text".to_string(), Value::String(text));
    }
    for (key, value) in children {
        match map.get_mut(&key) {
            None => {
                map.insert(key, value);
            }
            Some(existing) => {
                if let Value::Array(items) = existing {
                    items.push(value);
                } else {
                    let previous = map.remove(&key).expect("existing key");
                    map.insert(key, Value::Array(vec![previous, value]));
                }
            }
        }
    }
    Value::Object(map)
}

fn qname_to_string(raw: &[u8]) -> NetconfClientResult<String> {
    String::from_utf8(raw.to_vec())
        .map_err(|err| NetconfClientError::new(format!("non-utf8 xml name: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_format_tokens() {
        assert_eq!(parse_format("").unwrap(), Format::default());
        assert_eq!(
            parse_format("xml").unwrap(),
            Format {
                kind: FormatKind::Xml,
                pretty: false,
                unescape: false
            }
        );
        assert_eq!(
            parse_format("pretty,unescape").unwrap(),
            Format {
                kind: FormatKind::Xml,
                pretty: true,
                unescape: true
            }
        );
        assert_eq!(
            parse_format("json, pretty").unwrap(),
            Format {
                kind: FormatKind::Json,
                pretty: true,
                unescape: false
            }
        );
        assert!(parse_format("xml,json").unwrap_err().contains("mix"));
        assert!(parse_format("yaml").unwrap_err().contains("unknown"));
    }

    #[test]
    fn pretty_xml_indents() {
        let out = pretty_xml("<root><a>1</a><b/></root>").unwrap();
        assert_eq!(out, "<root>\n  <a>1</a>\n  <b/>\n</root>");
    }

    #[test]
    fn unescape_then_pretty() {
        let format = parse_format("pretty,unescape").unwrap();
        let out = format
            .render("&lt;root&gt;&lt;a/&gt;&lt;/root&gt;")
            .unwrap();
        assert_eq!(out, "<root>\n  <a/>\n</root>");
    }

    #[test]
    fn xml_to_json_rpc_reply() {
        let xml = r#"<rpc-reply xmlns="urn:ietf:params:xml:ns:netconf:base:1.0" message-id="1"><data><ok/></data></rpc-reply>"#;
        assert_eq!(
            xml_to_json(xml).unwrap(),
            json!({
                "rpc-reply": {
                    "@xmlns": "urn:ietf:params:xml:ns:netconf:base:1.0",
                    "@message-id": "1",
                    "data": { "ok": {} }
                }
            })
        );
    }

    #[test]
    fn xml_to_json_repeated_children_become_array() {
        let xml = "<root><item>a</item><item>b</item></root>";
        assert_eq!(
            xml_to_json(xml).unwrap(),
            json!({ "root": { "item": ["a", "b"] } })
        );
    }

    #[test]
    fn host_filename_sanitizes() {
        assert_eq!(host_filename("r1.example", "xml"), "r1.example.xml");
        assert_eq!(
            host_filename("192.0.2.10:830", "json"),
            "192.0.2.10_830.json"
        );
        assert_eq!(
            host_filename("[2001:db8::1]:830", "xml"),
            "2001_db8__1_830.xml"
        );
        assert_eq!(host_filename("   ", "xml"), "host.xml");
    }

    #[test]
    fn output_dir_writes_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        let output = Output::new(Format::default(), Some(dir.path().to_path_buf()));
        output.emit("r1", "<a/>").unwrap();
        output.emit("r1", "<b/>").unwrap();
        let body = std::fs::read_to_string(dir.path().join("r1.xml")).unwrap();
        assert_eq!(body, "<a/>\n<b/>\n");
    }
}
