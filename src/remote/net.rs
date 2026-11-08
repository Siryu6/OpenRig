//! Who may talk to the web remote: clients on the local network only, the
//! pedal reached by its address or a local name (no DNS rebinding), and
//! requests that come from the remote's own page (no cross-site requests).

use std::net::IpAddr;

/// Clients on a local network: private IPv4 (RFC 1918, which includes the
/// pedal's hotspot 10.42.0.0/24), link-local, IPv6 unique local and
/// link-local, and the pedal itself (an SSH tunnel)
pub fn is_local_client(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local_client(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                // fc00::/7
                || first & 0xfe00 == 0xfc00
                // fe80::/10
                || first & 0xffc0 == 0xfe80
        }
    }
}

/// The host part of a Host header value ("10.42.0.1:8080" → "10.42.0.1",
/// "[fe80::1]:80" → "fe80::1"), lowercased
fn host_name(host: &str) -> String {
    let host = host.trim();
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
            _ => host,
        }
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether the Host header names the pedal: an IP address, or one of its
/// local names (`names`, e.g. "openrig.local", "pedalboard"). Any other
/// name is a page of another site resolving its name to the pedal.
pub fn is_allowed_host(host: &str, names: &[String]) -> bool {
    let name = host_name(host);
    if name.is_empty() {
        return false;
    }
    if name.parse::<IpAddr>().is_ok() {
        return true;
    }
    names.iter().any(|n| n.eq_ignore_ascii_case(&name))
}

/// Whether a request comes from the remote's own page: its Origin is the
/// address it was sent to (`http://<Host>`)
pub fn is_same_origin(origin: Option<&str>, host: &str) -> bool {
    let Some(origin) = origin else {
        return false;
    };
    let origin = origin.trim().to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    // Browsers leave the default port out of Origin, not always of Host
    let strip_80 = |s: &str| s.strip_suffix(":80").unwrap_or(s).to_string();
    strip_80(rest) == strip_80(&host)
}

/// The local names of the pedal: openrig.local and its host name, with and
/// without .local, and localhost
pub fn local_names(hostname: &str) -> Vec<String> {
    let mut names = vec![
        "openrig.local".to_string(),
        "openrig".to_string(),
        "localhost".to_string(),
    ];
    let hostname = hostname.trim().to_ascii_lowercase();
    if !hostname.is_empty() {
        names.push(format!("{hostname}.local"));
        names.push(hostname);
    }
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse()
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::BROADCAST))
    }

    #[test]
    fn only_local_clients() {
        for ok in [
            "10.42.0.23",
            "192.168.1.10",
            "172.16.4.4",
            "172.31.255.1",
            "169.254.1.2",
            "127.0.0.1",
            "::1",
            "fd12::1",
            "fe80::1234",
            "::ffff:192.168.0.3",
        ] {
            assert!(is_local_client(ip(ok)), "{ok}");
        }
        for refused in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "2001:db8::1",
            "::ffff:1.1.1.1",
            "255.255.255.255",
        ] {
            assert!(!is_local_client(ip(refused)), "{refused}");
        }
    }

    #[test]
    fn host_names_the_pedal() {
        let names = local_names("pedalboard");
        for ok in [
            "10.42.0.1",
            "10.42.0.1:80",
            "192.168.1.5:8080",
            "[fe80::1]:80",
            "[::1]",
            "openrig.local",
            "OpenRig.local:80",
            "pedalboard.local",
            "pedalboard",
            "localhost:8080",
        ] {
            assert!(is_allowed_host(ok, &names), "{ok}");
        }
        for refused in [
            "evil.example",
            "openrig.local.evil.example",
            "",
            ":80",
            "10.0.0.1.nip.io",
        ] {
            assert!(!is_allowed_host(refused, &names), "{refused}");
        }
    }

    #[test]
    fn origin_is_the_pedal() {
        assert!(is_same_origin(Some("http://10.42.0.1"), "10.42.0.1"));
        assert!(is_same_origin(Some("http://10.42.0.1"), "10.42.0.1:80"));
        assert!(is_same_origin(
            Some("http://openrig.local:8080"),
            "OpenRig.local:8080"
        ));
        assert!(!is_same_origin(None, "10.42.0.1"));
        assert!(!is_same_origin(Some("null"), "10.42.0.1"));
        assert!(!is_same_origin(Some("http://evil.example"), "10.42.0.1"));
        assert!(!is_same_origin(Some("http://10.42.0.1:8080"), "10.42.0.1"));
    }
}
