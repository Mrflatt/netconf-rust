use dirs::home_dir;
use netconf_async::error::{NetconfClientError, NetconfClientResult};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// One fan-out target: address plus optional template variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub address: String,
    pub vars: Vars,
}

/// Per-host template data. A repeated address becomes [`Vars::List`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Vars {
    #[default]
    None,
    Map(BTreeMap<String, String>),
    List(Vec<BTreeMap<String, String>>),
}

impl Vars {
    fn push_row(&mut self, row: BTreeMap<String, String>) {
        match self {
            Self::None => *self = Self::Map(row),
            Self::Map(_) => {
                if let Self::Map(first) = std::mem::replace(self, Self::None) {
                    *self = Self::List(vec![first, row]);
                }
            }
            Self::List(rows) => rows.push(row),
        }
    }

    pub fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Expand `--host` values: bare addresses and `@file.csv` inventories.
pub fn expand_hosts(values: &[&str], delimiter: u8) -> NetconfClientResult<Vec<Target>> {
    let mut targets = Vec::new();
    for value in values {
        if let Some(path) = value.strip_prefix('@') {
            let from_file = read_inventory_file(path, delimiter)?;
            for target in from_file {
                merge_target(&mut targets, target);
            }
        } else {
            let address = value.trim();
            if address.is_empty() {
                return Err(NetconfClientError::new("empty --host value".to_string()));
            }
            merge_target(
                &mut targets,
                Target {
                    address: address.to_string(),
                    vars: Vars::None,
                },
            );
        }
    }
    if targets.is_empty() {
        return Err(NetconfClientError::new(
            "host required (--host or --host @file.csv)".to_string(),
        ));
    }
    Ok(targets)
}

pub fn parse_delimiter(value: &str) -> Result<u8, String> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => Ok(c as u8),
        _ => Err("delimiter must be a single ASCII character".to_string()),
    }
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("/"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let mut home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.push(rest);
        return home;
    }
    path.to_path_buf()
}

fn read_inventory_file(path: &str, delimiter: u8) -> NetconfClientResult<Vec<Target>> {
    let expanded = expand_tilde(Path::new(path));
    let file = std::fs::File::open(&expanded).map_err(|err| {
        NetconfClientError::new(format!(
            "failed to read inventory '{}': {err}",
            expanded.display()
        ))
    })?;
    parse_inventory(file, delimiter).map_err(|err| {
        NetconfClientError::new(format!(
            "failed to parse inventory '{}': {err}",
            expanded.display()
        ))
    })
}

fn parse_inventory<R: Read>(reader: R, delimiter: u8) -> NetconfClientResult<Vec<Target>> {
    let mut csv = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(reader);
    let mut records = csv.records();
    let Some(first) = records.next() else {
        return Err(NetconfClientError::new(
            "inventory file is empty".to_string(),
        ));
    };
    let first = record(first)?;

    let mut targets = Vec::new();
    if let Some(host_index) = host_column(&first) {
        let keys = first;
        for row in records {
            let row = record(row)?;
            if row.is_empty() || row.iter().all(|cell| cell.is_empty()) {
                continue;
            }
            let Some(address) = row.get(host_index).map(String::as_str) else {
                continue;
            };
            if address.is_empty() {
                continue;
            }
            let mut vars = BTreeMap::new();
            for (idx, key) in keys.iter().enumerate() {
                vars.insert(key.clone(), row.get(idx).cloned().unwrap_or_default());
            }
            merge_target(
                &mut targets,
                Target {
                    address: address.to_string(),
                    vars: Vars::Map(vars),
                },
            );
        }
        return Ok(targets);
    }

    if first.len() != 1 {
        return Err(NetconfClientError::new(
            "inventory missing required column 'ip' or 'host'".to_string(),
        ));
    }

    // Bare host list: first row is a host unless it is a lone ip/host header.
    if first[0] != "ip" && first[0] != "host" {
        merge_target(
            &mut targets,
            Target {
                address: first[0].clone(),
                vars: Vars::None,
            },
        );
    }
    for row in records {
        let row = record(row)?;
        let Some(address) = row.first().map(String::as_str) else {
            continue;
        };
        if address.is_empty() {
            continue;
        }
        merge_target(
            &mut targets,
            Target {
                address: address.to_string(),
                vars: Vars::None,
            },
        );
    }
    Ok(targets)
}

fn host_column(header: &[String]) -> Option<usize> {
    header
        .iter()
        .position(|name| name == "ip")
        .or_else(|| header.iter().position(|name| name == "host"))
}

fn record(row: Result<csv::StringRecord, csv::Error>) -> NetconfClientResult<Vec<String>> {
    let row = row.map_err(|err| NetconfClientError::new(format!("invalid csv: {err}")))?;
    Ok(row.iter().map(str::to_string).collect())
}

fn merge_target(targets: &mut Vec<Target>, incoming: Target) {
    let key = host_key(&incoming.address);
    if let Some(existing) = targets
        .iter_mut()
        .find(|target| host_key(&target.address) == key)
    {
        match incoming.vars {
            Vars::None => {}
            Vars::Map(row) => existing.vars.push_row(row),
            Vars::List(rows) => {
                for row in rows {
                    existing.vars.push_row(row);
                }
            }
        }
        return;
    }
    targets.push(incoming);
}

/// Grouping key: host without a trailing `:port`. Bare IPv6 stays intact.
pub(crate) fn host_key(addr: &str) -> String {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix('[')
        && let Some((host, _)) = rest.split_once(']')
        && !host.is_empty()
    {
        return host.to_string();
    }
    if addr.matches(':').count() > 1 {
        return addr.to_string();
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            host.to_string()
        }
        _ => addr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(csv: &str, delimiter: u8) -> Vec<Target> {
        parse_inventory(Cursor::new(csv), delimiter).unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn parse_hosts_with_variables_and_duplicate_ip() {
        let csv = r#"ip,port,lag
"192.0.2.1","1/1/1","lag-1"
"192.0.2.2","1/1/2","lag-2"
"192.0.2.3:830","1/1/3","lag-3"
192.0.2.4,1/1/4,lag-4
192.0.2.1,1/1/2,"lag-2"
2001:db8::10,1/1/5,lag-5
[2001:db8::20]:830,1/1/6,lag-6
[2001:db8::10]:830,1/1/7,lag-7
"#;
        let hosts = parse(csv, b',');
        assert_eq!(hosts.len(), 6);
        assert_eq!(hosts[0].address, "192.0.2.1");
        assert_eq!(
            hosts[0].vars,
            Vars::List(vec![
                map(&[("ip", "192.0.2.1"), ("lag", "lag-1"), ("port", "1/1/1")]),
                map(&[("ip", "192.0.2.1"), ("lag", "lag-2"), ("port", "1/1/2")]),
            ])
        );
        assert_eq!(hosts[1].address, "192.0.2.2");
        assert_eq!(hosts[2].address, "192.0.2.3:830");
        assert_eq!(host_key(&hosts[2].address), "192.0.2.3");
        assert_eq!(hosts[3].address, "192.0.2.4");
        assert_eq!(hosts[4].address, "2001:db8::10");
        assert_eq!(host_key(&hosts[4].address), "2001:db8::10");
        assert_eq!(
            hosts[4].vars,
            Vars::List(vec![
                map(&[("ip", "2001:db8::10"), ("lag", "lag-5"), ("port", "1/1/5"),]),
                map(&[
                    ("ip", "[2001:db8::10]:830"),
                    ("lag", "lag-7"),
                    ("port", "1/1/7"),
                ]),
            ])
        );
        assert_eq!(hosts[5].address, "[2001:db8::20]:830");
        assert_eq!(host_key(&hosts[5].address), "2001:db8::20");
    }

    #[test]
    fn parse_hosts_without_header() {
        let hosts = parse(
            "192.0.2.1\n192.0.2.2\n2001:db8::10\n[2001:db8::20]:830\n",
            b',',
        );
        assert_eq!(
            hosts.iter().map(|h| h.address.as_str()).collect::<Vec<_>>(),
            [
                "192.0.2.1",
                "192.0.2.2",
                "2001:db8::10",
                "[2001:db8::20]:830",
            ]
        );
        assert!(hosts.iter().all(|h| h.vars == Vars::None));
    }

    #[test]
    fn parse_hosts_custom_delimiter() {
        let csv = "host;serviceId;description\n192.0.2.1;100001;testing 1\n";
        let hosts = parse(csv, b';');
        assert_eq!(hosts.len(), 1);
        assert_eq!(
            hosts[0].vars,
            Vars::Map(map(&[
                ("host", "192.0.2.1"),
                ("serviceId", "100001"),
                ("description", "testing 1"),
            ]))
        );
    }

    #[test]
    fn parse_hosts_requires_ip_or_host_when_multicolumn() {
        let err = parse_inventory(Cursor::new("foo,bar\n1,2\n"), b',').unwrap_err();
        assert!(err.to_string().contains("ip' or 'host"), "{err}");
    }

    #[test]
    fn merge_normalizes_port_and_ipv6() {
        let mut values = expand_hosts(&["192.0.2.10", "192.0.2.10:830"], b',').unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].address, "192.0.2.10");

        values = expand_hosts(&["[2001:db8::1]:830", "2001:db8::1"], b',').unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].address, "[2001:db8::1]:830");
    }

    #[test]
    fn expand_hosts_reads_at_file_and_bare_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.csv");
        std::fs::write(&path, "ip,role\n192.0.2.10,edge\n").unwrap();
        let at = format!("@{}", path.display());
        let hosts = expand_hosts(&["r1.example", &at], b',').unwrap();
        assert_eq!(hosts[0].address, "r1.example");
        assert_eq!(hosts[0].vars, Vars::None);
        assert_eq!(hosts[1].address, "192.0.2.10");
        assert_eq!(
            hosts[1].vars,
            Vars::Map(map(&[("ip", "192.0.2.10"), ("role", "edge")]))
        );
    }

    #[test]
    fn expand_hosts_rejects_empty() {
        let err = expand_hosts(&[], b',').unwrap_err();
        assert!(err.to_string().contains("host required"), "{err}");
    }

    #[test]
    fn parse_delimiter_single_ascii() {
        assert_eq!(parse_delimiter(",").unwrap(), b',');
        assert_eq!(parse_delimiter(";").unwrap(), b';');
        assert!(parse_delimiter("").is_err());
        assert!(parse_delimiter(",,").is_err());
    }
}
