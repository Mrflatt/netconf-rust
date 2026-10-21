use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Parsed allow-list. Empty means every host is accepted.
#[derive(Debug, Clone, Default)]
pub struct AllowedNets {
    nets: Vec<IpNet>,
}

#[derive(Debug, Clone)]
struct IpNet {
    addr: IpAddr,
    prefix: u8,
}

impl AllowedNets {
    /// No filter.
    pub fn allow_all() -> Self {
        Self { nets: Vec::new() }
    }

    /// Parse CIDR strings (`192.0.2.0/24`, `2001:db8::/32`).
    pub fn parse(cidrs: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, String> {
        let mut nets = Vec::new();
        for cidr in cidrs {
            let cidr = cidr.as_ref();
            nets.push(parse_cidr(cidr).ok_or_else(|| format!("invalid CIDR {cidr:?}"))?);
        }
        Ok(Self { nets })
    }

    /// True when no allow-list is configured.
    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    /// Accept `host` if it is an IP in a configured net, or a name that
    /// resolves to at least one such address.
    pub fn check(&self, host: &str) -> Result<(), String> {
        self.pin(host).map(|_| ())
    }

    /// Host string that is safe to dial.
    ///
    /// Empty allow-list leaves `host` unchanged. An IP literal must itself be
    /// in a net. A hostname is resolved and replaced with the first allowed
    /// address so a later lookup cannot land on a different, disallowed IP.
    pub fn pin(&self, host: &str) -> Result<String, String> {
        if self.nets.is_empty() {
            return Ok(host.to_string());
        }
        let host = destination_host(host);
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if self.contains(ip) {
                Ok(host.to_string())
            } else {
                Err(format!("host {host:?} is not in allowed subnets"))
            };
        }
        let ips = resolve_host(host)?;
        if ips.is_empty() {
            return Err(format!("failed to resolve host {host:?}"));
        }
        match ips.into_iter().find(|ip| self.contains(*ip)) {
            Some(ip) => Ok(ip.to_string()),
            None => Err(format!("host {host:?} is not in allowed subnets")),
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.nets.iter().any(|net| net.contains(ip))
    }
}

impl IpNet {
    fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(addr)) => {
                ipv4_prefix(net, self.prefix) == ipv4_prefix(addr, self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(addr)) => {
                ipv6_prefix(net, self.prefix) == ipv6_prefix(addr, self.prefix)
            }
            _ => false,
        }
    }
}

/// Host without a trailing `:port`. Bare IPv6 stays intact.
pub(crate) fn destination_host(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[')
        && let Some((inside, after)) = rest.split_once(']')
        && !inside.is_empty()
        && (after.is_empty() || after.starts_with(':'))
    {
        return inside;
    }
    if host.parse::<IpAddr>().is_ok() {
        return host;
    }
    if host.matches(':').count() == 1
        && let Some((name, port)) = host.rsplit_once(':')
        && !name.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
    {
        return name;
    }
    host
}

fn parse_cidr(cidr: &str) -> Option<IpNet> {
    let (addr, prefix) = cidr.split_once('/')?;
    let addr: IpAddr = addr.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return None;
    }
    Some(IpNet { addr, prefix })
}

fn resolve_host(host: &str) -> Result<Vec<IpAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    (host, 0u16)
        .to_socket_addrs()
        .map(|addrs| addrs.map(|addr| addr.ip()).collect())
        .map_err(|err| format!("failed to resolve host {host:?}: {err}"))
}

fn ipv4_prefix(addr: Ipv4Addr, prefix: u8) -> u32 {
    let bits = u32::from(addr);
    if prefix == 0 {
        0
    } else {
        bits & (!0u32 << (32 - prefix))
    }
}

fn ipv6_prefix(addr: Ipv6Addr, prefix: u8) -> u128 {
    let bits = u128::from(addr);
    if prefix == 0 {
        0
    } else {
        bits & (!0u128 << (128 - prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allow_list_accepts_everything() {
        let nets = AllowedNets::allow_all();
        assert!(nets.check("192.0.2.1").is_ok());
        assert!(nets.check("example.invalid").is_ok());
    }

    #[test]
    fn cidr_matches_address() {
        let nets = AllowedNets::parse(["192.0.2.0/24"]).unwrap();
        assert!(nets.check("192.0.2.10").is_ok());
        assert!(nets.check("198.51.100.1").is_err());
    }

    #[test]
    fn bad_cidr_is_rejected() {
        assert!(AllowedNets::parse(["not-a-cidr"]).is_err());
        assert!(AllowedNets::parse(["192.0.2.0/99"]).is_err());
    }

    #[test]
    fn ipv6_cidr() {
        let nets = AllowedNets::parse(["2001:db8::/32"]).unwrap();
        assert!(nets.check("2001:db8::1").is_ok());
        assert!(nets.check("2001:db9::1").is_err());
    }

    #[test]
    fn pin_keeps_allowed_ip_and_rejects_others() {
        let nets = AllowedNets::parse(["192.0.2.0/24"]).unwrap();
        assert_eq!(nets.pin("192.0.2.10").unwrap(), "192.0.2.10");
        assert!(nets.pin("198.51.100.1").is_err());
        assert_eq!(AllowedNets::allow_all().pin("alias").unwrap(), "alias");
    }

    #[test]
    fn pin_strips_port_before_check() {
        let nets = AllowedNets::parse(["192.0.2.0/24", "2001:db8::/32"]).unwrap();
        assert_eq!(nets.pin("192.0.2.10:830").unwrap(), "192.0.2.10");
        assert_eq!(nets.pin("[2001:db8::1]:830").unwrap(), "2001:db8::1");
        assert_eq!(destination_host("2001:db8::1"), "2001:db8::1");
        assert_eq!(destination_host("kutomo:830"), "kutomo");
    }
}
