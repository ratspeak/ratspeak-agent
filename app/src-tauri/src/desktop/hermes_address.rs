//! Where Setup reaches Hermes. Its API server speaks plain HTTP, so Setup connects only on this
//! computer, a private network, or a VPN, and pins each request to the addresses it checked.
use reqwest::blocking::Client;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

const EXAMPLE: &str = "Enter an address such as 192.168.1.20 or hermes.local.";

#[derive(Debug)]
pub(super) struct Target {
    name: String,
    addrs: Vec<SocketAddr>,
    port: u16,
    pub local: bool,
}

/// The address as Setup stores it, or None for this computer.
pub(super) fn normalize(host: Option<&str>) -> Result<Option<String>, String> {
    let lower = host.unwrap_or("").trim().to_ascii_lowercase();
    let raw = lower.strip_prefix("http://").unwrap_or(&lower).trim_end_matches('/');
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.contains("://") || raw.contains('/') || raw.contains('@') {
        return Err(EXAMPLE.into());
    }
    let bare = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(raw);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok((!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string()));
    }
    let name = bare.strip_suffix('.').unwrap_or(bare);
    let label = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    if name.len() > 253 || !name.split('.').all(label) {
        return Err(EXAMPLE.into());
    }
    Ok((name != "localhost").then(|| name.to_owned()))
}

/// Loopback, RFC 1918, carrier-grade NAT (Tailscale), and IPv6 unique local addresses.
fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || (a == 100 && b & 0xc0 == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.segments()[0] & 0xfe00 == 0xfc00
                || v6.to_ipv4_mapped().is_some_and(|v4| private(IpAddr::V4(v4)))
        }
    }
}

pub(super) fn target(host: Option<&str>, port: u16) -> Result<Target, String> {
    let Some(name) = normalize(host)? else {
        return Ok(Target { name: "127.0.0.1".into(), addrs: Vec::new(), port, local: true });
    };
    let missing = || format!("Couldn’t find {name} on your network.");
    let found: Vec<SocketAddr> = (name.as_str(), port).to_socket_addrs().map_err(|_| missing())?.collect();
    // A .local name can also resolve to link-local addresses; only the checked ones are used.
    let addrs: Vec<SocketAddr> = found.iter().copied().filter(|a| private(a.ip())).collect();
    if addrs.is_empty() {
        return Err(if found.is_empty() {
            missing()
        } else {
            format!("{name} isn’t on your local network. Hermes’s API isn’t encrypted, so Ratspeak Agent connects to it only on your local network or a VPN.")
        });
    }
    Ok(Target { name, addrs, port, local: false })
}

impl Target {
    fn authority(&self) -> String {
        if self.name.contains(':') {
            format!("[{}]:{}", self.name, self.port)
        } else {
            format!("{}:{}", self.name, self.port)
        }
    }

    pub fn url(&self, profile: Option<&str>, path: &str) -> Result<String, String> {
        let base = format!("http://{}", self.authority());
        let Some(name) = profile else {
            return Ok(format!("{base}/{path}"));
        };
        // Hermes profile IDs are one URL segment, up to 64 lowercase ASCII characters.
        let id_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        if name.len() > 64
            || !name.bytes().next().is_some_and(id_char)
            || !name.bytes().all(|b| id_char(b) || b == b'_' || b == b'-')
        {
            return Err("Enter a valid Hermes profile name.".into());
        }
        Ok(format!("{base}/p/{name}/{path}"))
    }

    pub fn client(&self, connect: Duration, total: Duration) -> Result<Client, String> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(connect)
            .timeout(total);
        if !self.local && self.name.parse::<IpAddr>().is_err() {
            builder = builder.resolve_to_addrs(&self.name, &self.addrs);
        }
        builder.build().map_err(|_| "Couldn’t open a connection to Hermes.".into())
    }

    pub fn unreachable(&self) -> String {
        if self.local {
            return "Couldn’t reach Hermes. Enable its API server and start the gateway on this computer.".into();
        }
        let here = self.addrs.first().and_then(|peer| this_computer(*peer))
            .map_or("this computer".into(), |ip| format!("this computer ({ip})"));
        format!("Couldn’t reach Hermes at {}. On that computer, set API_SERVER_HOST so Hermes listens on its network address, and allow {here} through its firewall.", self.authority())
    }
}

/// The address this computer would use to reach `peer`. Connecting a UDP socket sends nothing.
fn this_computer(peer: SocketAddr) -> Option<IpAddr> {
    let any: IpAddr = if peer.is_ipv4() { Ipv4Addr::UNSPECIFIED.into() } else { Ipv6Addr::UNSPECIFIED.into() };
    let socket = UdpSocket::bind((any, 0)).ok()?;
    socket.connect(peer).ok()?;
    Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer_is_the_default_and_other_addresses_are_normalized() {
        for here in ["", "  ", "localhost", "LOCALHOST.", "127.0.0.1", "[::1]", "http://localhost/", "0.0.0.0"] {
            assert_eq!(normalize(Some(here)).unwrap(), None, "{here}");
        }
        assert_eq!(normalize(None).unwrap(), None);
        for (typed, stored) in [
            ("10.1.1.50", "10.1.1.50"),
            ("http://10.1.1.50/", "10.1.1.50"),
            ("Agent.Local.", "agent.local"),
            ("[FD7A:115C:A1E0::1]", "fd7a:115c:a1e0::1"),
            ("hermes-vm", "hermes-vm"),
        ] {
            assert_eq!(normalize(Some(typed)).unwrap().as_deref(), Some(stored), "{typed}");
        }
        for wrong in ["https://agent.local", "agent.local/v1", "user@agent", "bad host", "a..b", "-agent", "agent_vm"] {
            assert!(normalize(Some(wrong)).is_err(), "{wrong}");
        }
    }

    #[test]
    fn plain_http_stays_on_private_networks_and_vpns() {
        for ok in ["10.1.1.50", "172.16.0.1", "192.168.1.2", "100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1", "::ffff:10.0.0.1", "127.0.0.1"] {
            assert!(private(ok.parse().unwrap()), "{ok}");
        }
        for public in ["8.8.8.8", "100.128.0.1", "169.254.169.254", "172.32.0.1", "fe80::1", "2001:db8::1"] {
            assert!(!private(public.parse().unwrap()), "{public}");
        }
        assert!(target(Some("8.8.8.8"), 8642).unwrap_err().contains("isn’t on your local network"));
        assert!(target(Some("agent.invalid"), 8642).unwrap_err().contains("Couldn’t find agent.invalid"));
    }

    #[test]
    fn urls_and_messages_name_the_gateway() {
        let local = target(None, 8642).unwrap();
        assert_eq!(local.url(None, "health").unwrap(), "http://127.0.0.1:8642/health");
        assert!(local.unreachable().contains("on this computer"));
        let lan = target(Some("10.1.1.50"), 8642).unwrap();
        assert_eq!(lan.url(Some("work"), "v1/capabilities").unwrap(), "http://10.1.1.50:8642/p/work/v1/capabilities");
        assert!(lan.url(Some("Work"), "health").is_err());
        let message = lan.unreachable();
        assert!(message.starts_with("Couldn’t reach Hermes at 10.1.1.50:8642. On that computer, set API_SERVER_HOST"), "{message}");
        assert!(message.contains("this computer"));
        let v6 = target(Some("fd7a:115c:a1e0::1"), 8642).unwrap();
        assert_eq!(v6.url(None, "health").unwrap(), "http://[fd7a:115c:a1e0::1]:8642/health");
    }

    #[test]
    fn a_hostname_connects_only_to_the_addresses_it_was_checked_against() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let headers: Vec<String> = BufReader::new(&stream).lines().map(Result::unwrap).take_while(|s| !s.is_empty()).collect();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            headers
        });
        // hermes.invalid never resolves, so the request can only reach the pinned address.
        let pinned = Target { name: "hermes.invalid".into(), addrs: vec![([127, 0, 0, 1], port).into()], port, local: false };
        let client = pinned.client(Duration::from_secs(2), Duration::from_secs(2)).unwrap();
        assert_eq!(client.get(pinned.url(None, "health").unwrap()).send().unwrap().text().unwrap(), "ok");
        let headers = server.join().unwrap();
        assert_eq!(headers[0], "GET /health HTTP/1.1");
        assert!(headers.iter().any(|h| h.eq_ignore_ascii_case(&format!("host: hermes.invalid:{port}"))));
    }
}
