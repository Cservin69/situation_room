//! URL validation guarding against SSRF (Server-Side Request Forgery).
//!
//! Every outbound HTTP request that involves a user-controlled or
//! ingested URL passes through [`UrlGuard::check`] before being issued.
//! If we ever fetch a URL the user entered (article archiving, "fetch
//! this source"), this is the difference between a safe feature and an
//! attacker reading `http://169.254.169.254/latest/meta-data/`.
//!
//! ## What we reject
//!
//! - Non-HTTP(S) schemes: `file://`, `ftp://`, `gopher://`, `dict://`, `data:`, etc.
//! - Hosts resolving to private IP space (RFC 1918, RFC 4193, link-local).
//! - Cloud metadata endpoints (AWS/GCP/Azure IMDS: 169.254.169.254 and friends).
//! - `localhost`, `0.0.0.0`, `::1`.
//! - Ports outside the allowlist (default: 80, 443).
//! - URLs with embedded credentials (`http://user:pass@host/`).
//!
//! ## What we don't do
//!
//! We don't resolve DNS here — that's a TOCTOU race (attacker's DNS could
//! return a public IP at check time and a private IP at fetch time).
//! The actual DNS resolution + post-resolution IP check happens inside the
//! [`crate::http::SecureHttpClient`] at connect time using a custom resolver.


use std::net::IpAddr;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum UrlViolation {
    #[error("url parse failed: {0}")]
    Parse(String),
    #[error("scheme not allowed: {0} (only http/https)")]
    BadScheme(String),
    #[error("host missing from url")]
    NoHost,
    #[error("host is a private or special-use IP: {0}")]
    PrivateIp(String),
    #[error("host is localhost-like: {0}")]
    Localhost(String),
    #[error("port not in allowlist: {0}")]
    BadPort(u16),
    #[error("url contains embedded credentials")]
    EmbeddedCredentials,
    #[error("url too long: {0} bytes (max {1})")]
    TooLong(usize, usize),
}

pub struct UrlGuard {
    allowed_ports: Vec<u16>,
    max_url_length: usize,
}

impl Default for UrlGuard {
    fn default() -> Self {
        Self {
            allowed_ports: vec![80, 443],
            max_url_length: 2048,
        }
    }
}

impl UrlGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_allowed_ports(mut self, ports: Vec<u16>) -> Self {
        self.allowed_ports = ports;
        self
    }

    /// Validate a URL string. Returns the parsed [`Url`] on success.
    pub fn check(&self, input: &str) -> Result<Url, UrlViolation> {
        if input.len() > self.max_url_length {
            return Err(UrlViolation::TooLong(input.len(), self.max_url_length));
        }

        let url = Url::parse(input).map_err(|e| UrlViolation::Parse(e.to_string()))?;

        // 1. Scheme
        match url.scheme() {
            "http" | "https" => {}
            other => return Err(UrlViolation::BadScheme(other.to_string())),
        }

        // 2. Embedded credentials (http://user:pass@host/)
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UrlViolation::EmbeddedCredentials);
        }

        // 3. Host must be present
        let host = url.host_str().ok_or(UrlViolation::NoHost)?;

        // 4. Reject explicitly localhost-like hostnames
        let host_lower = host.to_ascii_lowercase();
        if matches!(host_lower.as_str(), "localhost" | "localhost.localdomain") {
            return Err(UrlViolation::Localhost(host.to_string()));
        }

        // 5. If host is a literal IP, check it's not private / metadata / loopback.
        //
        // We use the typed `url::Host` variant rather than parsing the
        // output of `host_str()` — the string form of an IPv6 address
        // carries `[]` brackets (`"[::1]"`) which fail `IpAddr::from_str`
        // and silently bypass this check. That was a real bug caught by
        // `rejects_ipv6_loopback`.
        match url.host() {
            Some(url::Host::Ipv4(v4)) => {
                let ip = IpAddr::V4(v4);
                if is_disallowed_ip(&ip) {
                    return Err(UrlViolation::PrivateIp(ip.to_string()));
                }
            }
            Some(url::Host::Ipv6(v6)) => {
                let ip = IpAddr::V6(v6);
                if is_disallowed_ip(&ip) {
                    return Err(UrlViolation::PrivateIp(ip.to_string()));
                }
            }
            Some(url::Host::Domain(_)) => {
                // Domain name: DNS resolution happens later in the HTTP
                // client, with a resolver that rechecks the resolved IPs.
                // Nothing to validate at the guard layer.
            }
            None => {
                // Already caught above as NoHost.
            }
        }
        // NOTE: if host is a domain name, DNS resolution happens later in the
        // HTTP client with a resolver that rechecks the resolved IPs.

        // 6. Port allowlist (default_port() covers implicit 80/443)
        let port = url.port_or_known_default().ok_or(UrlViolation::BadPort(0))?;
        if !self.allowed_ports.contains(&port) {
            return Err(UrlViolation::BadPort(port));
        }

        Ok(url)
    }
}

/// True if the IP is in a range we never want to make outbound requests to.
pub fn is_disallowed_ip(ip: &IpAddr) -> bool {
    // Cloud instance metadata endpoints — the classic SSRF target
    const METADATA_ADDRS: &[&str] = &[
        "169.254.169.254", // AWS, GCP, Azure, DigitalOcean
        "fd00:ec2::254",   // AWS IPv6
        "100.100.100.200", // Alibaba Cloud
    ];
    let ip_str = ip.to_string();
    if METADATA_ADDRS.iter().any(|m| *m == ip_str) {
        return true;
    }

    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // Carrier-grade NAT
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
                // Reserved for future use
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local addresses (fc00::/7)
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local (fe80::/10)
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped — check the embedded v4
                || is_ipv4_mapped_and_disallowed(v6)
        }
    }
}

fn is_ipv4_mapped_and_disallowed(v6: &std::net::Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_disallowed_ip(&IpAddr::V4(v4));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_file_scheme() {
        assert!(matches!(
            UrlGuard::new().check("file:///etc/passwd"),
            Err(UrlViolation::BadScheme(_))
        ));
    }

    #[test]
    fn rejects_metadata_ip() {
        assert!(matches!(
            UrlGuard::new().check("http://169.254.169.254/latest/meta-data/"),
            Err(UrlViolation::PrivateIp(_))
        ));
    }

    #[test]
    fn rejects_localhost() {
        assert!(matches!(
            UrlGuard::new().check("http://localhost/admin"),
            Err(UrlViolation::Localhost(_))
        ));
    }

    #[test]
    fn rejects_rfc1918() {
        assert!(matches!(
            UrlGuard::new().check("http://192.168.1.1/"),
            Err(UrlViolation::PrivateIp(_))
        ));
        assert!(matches!(
            UrlGuard::new().check("http://10.0.0.1/"),
            Err(UrlViolation::PrivateIp(_))
        ));
    }

    #[test]
    fn rejects_embedded_creds() {
        assert!(matches!(
            UrlGuard::new().check("http://attacker:pass@example.com/"),
            Err(UrlViolation::EmbeddedCredentials)
        ));
    }

    #[test]
    fn rejects_bad_port() {
        assert!(matches!(
            UrlGuard::new().check("http://example.com:22/"),
            Err(UrlViolation::BadPort(22))
        ));
    }

    #[test]
    fn accepts_ordinary_https() {
        assert!(UrlGuard::new().check("https://api.example.com/data").is_ok());
    }

    #[test]
    fn rejects_ipv6_loopback() {
        assert!(matches!(
            UrlGuard::new().check("http://[::1]/"),
            Err(UrlViolation::PrivateIp(_))
        ));
    }
}
