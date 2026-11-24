//! SSRF-hardened HTTP client for remote imports ("give me a URL, store the file").
//!
//! An import endpoint makes the server fetch arbitrary URLs on behalf of an API key,
//! which is the classic SSRF setup. Defences, in order:
//! * only http/https, no userinfo in the URL;
//! * IP-literal hosts and every DNS answer must be globally routable: loopback,
//!   private, link-local (cloud metadata 169.254.169.254), CGNAT, multicast,
//!   documentation, IPv4-mapped/NAT64 forms of those etc. are refused;
//! * filtering happens inside the resolver the connection actually uses, so a DNS
//!   rebinding answer cannot slip a private IP in between check and connect;
//! * every redirect hop is re-validated (max 5), system proxies are ignored;
//! * connect/read timeouts; size is capped by the caller while streaming.

use anyhow::{bail, Result};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub fn is_global_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0                                   // 0.0.0.0/8
        || (o[0] == 100 && (o[1] & 0xC0) == 64)        // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24 IETF
        || (o[0] == 198 && (o[1] & 0xFE) == 18)        // 198.18.0.0/15 benchmarking
        || o[0] >= 240)                                // 240.0.0.0/4 reserved
}

pub fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_v4(v6) {
                return is_global_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xFE00) == 0xFC00                // fc00::/7 unique local
                || (s[0] & 0xFFC0) == 0xFE80                // fe80::/10 link local
                || (s[0] & 0xFFC0) == 0xFEC0                // fec0::/10 site local (deprecated)
                || (s[0] == 0x2001 && s[1] == 0x0DB8)       // documentation
                || (s[0] == 0x2001 && s[1] < 0x0200)        // 2001::/23 IETF special (Teredo etc.)
                || s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // discard-only
        }
    }
}

/// IPv4 addresses smuggled inside IPv6: mapped (::ffff:a.b.c.d), compatible (::a.b.c.d),
/// NAT64 (64:ff9b::/96) and 6to4 (2002:AABB:CCDD::).
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = v6.segments();
    let tail = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if s[..5] == [0, 0, 0, 0, 0] && (s[5] == 0xFFFF || (s[5] == 0 && !(s[6] == 0 && s[7] <= 1))) {
        return Some(tail);
    }
    if s[0] == 0x64 && s[1] == 0xFF9B && s[2..6] == [0, 0, 0, 0] {
        return Some(tail);
    }
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8));
    }
    None
}

struct GlobalOnlyResolver;

impl Resolve for GlobalOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = addrs.into_iter().filter(|a| is_global(a.ip())).collect();
            if allowed.is_empty() {
                let e: Box<dyn std::error::Error + Send + Sync> = format!("{host} resolves only to private or reserved addresses").into();
                return Err(e);
            }
            let it: Addrs = Box::new(allowed.into_iter());
            Ok(it)
        })
    }
}

/// Validate a URL before (and on every redirect to) it.
pub fn check_url(url: &reqwest::Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http and https URLs can be imported");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("put credentials in headers, not in the URL");
    }
    match url.host() {
        None => bail!("URL has no host"),
        Some(url::Host::Ipv4(ip)) if !is_global_v4(ip) => bail!("that address is private or reserved"),
        Some(url::Host::Ipv6(ip)) if !is_global(IpAddr::V6(ip)) => bail!("that address is private or reserved"),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") || d.ends_with(".internal") {
                bail!("that host name is internal");
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(Arc::new(GlobalOnlyResolver))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if let Err(e) = check_url(attempt.url()) {
                attempt.error(e.to_string())
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .user_agent("tgvault-import/0.1")
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> bool {
        is_global(s.parse().unwrap())
    }

    #[test]
    fn blocks_internal_addresses() {
        for bad in [
            "127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "255.255.255.255",
            "224.0.0.1", "192.0.2.10", "198.18.0.1", "240.0.0.1", "::1", "::", "fc00::1", "fd12::1", "fe80::1", "ff02::1",
            "::ffff:127.0.0.1", "::ffff:169.254.169.254", "64:ff9b::a9fe:a9fe", "2002:7f00:1::", "2001:db8::1", "::127.0.0.1",
        ] {
            assert!(!g(bad), "{bad} should be blocked");
        }
        for good in ["8.8.8.8", "1.1.1.1", "142.250.72.14", "2606:4700:4700::1111", "::ffff:8.8.8.8", "2002:0808:0808::"] {
            assert!(g(good), "{good} should be allowed");
        }
    }

    #[test]
    fn url_checks() {
        let ok = |u: &str| check_url(&u.parse().unwrap()).is_ok();
        assert!(ok("https://cdn.example.com/a.mp4"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("ftp://example.com/a"));
        assert!(!ok("http://127.0.0.1:8787/api"));
        assert!(!ok("http://[::1]/"));
        assert!(!ok("http://169.254.169.254/latest/meta-data/"));
        assert!(!ok("http://localhost/"));
        assert!(!ok("http://metadata.google.internal/"));
        assert!(!ok("https://user:pw@example.com/a"));
        assert!(!ok("http://0x7f000001/")); // parsed by the URL crate as 127.0.0.1
        assert!(!ok("http://2130706433/")); // decimal form of 127.0.0.1
    }
}
