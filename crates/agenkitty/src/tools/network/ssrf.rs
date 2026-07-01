//! The SSRF guard — the network tools' trust boundary.
//!
//! Every outbound request (and, later, every redirect hop) passes through
//! [`guard`], which implements **resolve → validate → pin**:
//!
//! 1. [`validate_url`] parses + canonicalizes the URL, rejects userinfo /
//!    non-http(s) / over-length, applies internal-name rules, normalizes the
//!    host (so a later pin matches the host the client will actually use), and —
//!    for IP literals — checks the deny-list directly (no DNS needed).
//! 2. The host is resolved **once** through an injected [`Resolve`]r.
//! 3. **Every** resolved address is checked against the hard-coded CIDR
//!    deny-list ([`ip_is_blocked`]); if *any* is blocked, the request is refused.
//! 4. The validated addresses are returned as a [`ValidatedTarget`] (which only
//!    [`guard`] can construct) so the HTTP client can be **pinned** to them and
//!    never re-resolve — defeating DNS rebinding (the dominant SSRF bypass).
//!
//! The deny-list is deliberately broader than "RFC1918 + metadata": it includes
//! IPv6 transition forms (NAT64 / 6to4 / Teredo / IPv4-mapped / IPv4-compatible),
//! CGNAT, TEST-NETs, reserved and deprecated ranges — the gaps that 2025–26 SSRF
//! advisories exploited. The network README tracks the deferred roadmap.

use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;

use ipnet::IpNet;
use pocopine_agenkit::server::BoxFuture;
use url::{Host, Url};

use super::error::{NetError, NetResult};

/// Maximum accepted URL length, in characters (matches Claude WebFetch).
pub const MAX_URL_LEN: usize = 250;

/// CIDR ranges that must never be reached. Built once. IPv4-mapped IPv6
/// (`::ffff:a.b.c.d`) is normalized to IPv4 *before* matching (see
/// [`ip_is_blocked`]), so it is intentionally absent here.
static BLOCKED_NETS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    const CIDRS: &[&str] = &[
        // ---- IPv4 ----
        "0.0.0.0/8",       // "this host" — 0.0.0.0 routes to localhost on Linux
        "127.0.0.0/8",     // loopback
        "10.0.0.0/8",      // RFC1918
        "172.16.0.0/12",   // RFC1918
        "192.168.0.0/16",  // RFC1918
        "169.254.0.0/16",  // link-local — AWS/GCP/Azure/DO metadata, ECS creds
        "100.64.0.0/10",   // CGNAT (covers Alibaba metadata 100.100.100.200)
        "192.0.0.0/24",    // IETF special-use (covers Oracle metadata 192.0.0.192)
        "192.0.2.0/24",    // TEST-NET-1
        "198.51.100.0/24", // TEST-NET-2
        "203.0.113.0/24",  // TEST-NET-3
        "198.18.0.0/15",   // benchmarking
        "192.88.99.0/24",  // 6to4 relay anycast (RFC 7526, deprecated)
        "224.0.0.0/4",     // multicast
        "240.0.0.0/4",     // reserved
        // ---- IPv6 ----
        "::1/128",        // loopback
        "::/96",          // IPv4-compatible (deprecated) + unspecified + loopback
        "100::/64",       // discard-only (RFC 6666)
        "fc00::/7",       // ULA (covers AWS IMDS-v6 fd00:ec2::254)
        "fe80::/10",      // link-local
        "fec0::/10",      // site-local (deprecated, RFC 3879)
        "64:ff9b::/96",   // NAT64 well-known (RFC 6052)
        "64:ff9b:1::/48", // NAT64 local-use (RFC 8215)
        "2002::/16",      // 6to4 (RFC 3056) — embeds IPv4
        "2001::/32",      // Teredo (RFC 4380)
        "2001:20::/28",   // ORCHIDv2 (RFC 7343)
        "2001:db8::/32",  // documentation (RFC 3849)
        "3fff::/32",      // documentation (RFC 9637)
        "5f00::/16",      // SRv6 SID (RFC 9602)
        "ff00::/8",       // multicast
    ];
    CIDRS
        .iter()
        .map(|cidr| cidr.parse().expect("static CIDR is valid"))
        .collect()
});

/// Whether an IP is in a blocked range. IPv4-mapped IPv6 is normalized to IPv4
/// first so `::ffff:127.0.0.1` is treated as `127.0.0.1` (and a mapped public
/// address like `::ffff:8.8.8.8` is correctly allowed).
pub fn ip_is_blocked(ip: IpAddr) -> bool {
    if let IpAddr::V6(v6) = ip
        && let Some(v4) = v6.to_ipv4_mapped()
    {
        return ip_is_blocked(IpAddr::V4(v4));
    }
    BLOCKED_NETS.iter().any(|net| net.contains(&ip))
}

/// Canonicalize a host to a stable identity: trimmed, trailing-dot stripped,
/// lowercased, and IDNA/punycode-encoded for internationalized domains. Used by
/// both the SSRF validator and [`super::policy::NetPolicy`] so the two layers
/// agree on what a host *is* — a divergence would let a request slip the
/// allowlist or wrongly refuse an allowlisted domain.
pub fn normalize_host(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.');
    match Host::parse(trimmed) {
        Ok(Host::Domain(domain)) => domain,
        Ok(other) => other.to_string(),
        Err(_) => trimmed.to_ascii_lowercase(),
    }
}

/// Internal hostnames that must never be reached, checked before resolution.
/// (The resolved-IP deny-check is the backstop; this catches internal names that
/// resolve via split-horizon DNS to an otherwise-allowed address.)
fn host_name_blocked(host: &str) -> bool {
    const EXACT: &[&str] = &[
        "localhost",
        "metadata.google.internal",
        "metadata.goog",
        "kubernetes",
        "kubernetes.default",
        "kubernetes.default.svc",
        "kubernetes.default.svc.cluster.local",
    ];
    const SUFFIX: &[&str] = &[
        ".localhost",
        ".internal",
        ".local",
        ".lan",
        ".corp",
        ".intranet",
        ".home.arpa",
    ];
    EXACT.contains(&host) || SUFFIX.iter().any(|suffix| host.ends_with(suffix))
}

/// A URL that passed structural + literal-IP validation but has not yet been
/// resolved (for domain hosts). The `url`'s host is normalized to equal `host`.
#[derive(Clone, Debug)]
pub struct UrlTarget {
    pub url: Url,
    /// Normalized host (lowercased, IDNA, trailing dot stripped).
    pub host: String,
    pub port: u16,
    /// `Some` if the host was an IP literal (already deny-checked).
    pub literal_ip: Option<IpAddr>,
}

/// Parse and structurally validate a URL. Does **no** DNS. Rejects malformed,
/// over-length, non-http(s), and userinfo-bearing URLs; applies internal-name
/// rules; deny-checks IP-literal hosts; and normalizes the URL's host so a later
/// connection pin keyed on the host matches the host the client will dial.
pub fn validate_url(raw: &str) -> NetResult<UrlTarget> {
    if raw.chars().count() > MAX_URL_LEN {
        return Err(NetError::url_too_long(format!(
            "URL exceeds {MAX_URL_LEN} characters"
        )));
    }
    let mut url = Url::parse(raw)
        .map_err(|err| NetError::invalid_url(format!("could not parse URL: {err}")))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(NetError::invalid_url(format!(
                "scheme `{other}` is not http(s)"
            )));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetError::invalid_url("URL must not contain userinfo"));
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| NetError::invalid_url("URL has no port"))?;

    // Extract owned host data so the immutable borrow ends before `set_host`.
    let parsed = match url.host() {
        Some(Host::Domain(domain)) => ParsedHost::Domain(domain.to_string()),
        Some(Host::Ipv4(addr)) => ParsedHost::Ip(IpAddr::V4(addr)),
        Some(Host::Ipv6(addr)) => ParsedHost::Ip(IpAddr::V6(addr)),
        None => return Err(NetError::invalid_url("URL has no host")),
    };

    let (host, literal_ip) = match parsed {
        ParsedHost::Domain(domain) => {
            let normalized = normalize_host(&domain);
            if host_name_blocked(&normalized) {
                return Err(NetError::not_allowed(format!(
                    "host `{normalized}` is a blocked internal name"
                )));
            }
            // Align the URL's host with the canonical form so a pin keyed on
            // `host` matches the host the HTTP client actually dials. Without
            // this, a trailing-dot (or otherwise non-canonical) host would make
            // the client re-resolve, reopening the DNS-rebinding window.
            if url.host_str() != Some(normalized.as_str()) {
                url.set_host(Some(&normalized))
                    .map_err(|_| NetError::invalid_url("could not normalize URL host"))?;
            }
            (normalized, None)
        }
        ParsedHost::Ip(ip) => {
            if ip_is_blocked(ip) {
                return Err(NetError::not_allowed(format!(
                    "address {ip} is in a blocked range"
                )));
            }
            (ip.to_string(), Some(ip))
        }
    };

    Ok(UrlTarget {
        url,
        host,
        port,
        literal_ip,
    })
}

enum ParsedHost {
    Domain(String),
    Ip(IpAddr),
}

/// An injected DNS resolver. Abstracted so the guard can be unit-tested without
/// real DNS (and so the production resolver — hickory, added in Phase 2 — stays
/// out of the security core's tests).
pub trait Resolve: Send + Sync {
    fn lookup(&self, host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>>;
}

/// A fully validated, pinnable target. The addresses here are the *only* ones the
/// HTTP client may connect to (via `reqwest`'s `resolve_to_addrs`), so it never
/// re-resolves. Fields are private and only [`guard`] constructs it: the type
/// itself is the proof that validation happened.
#[derive(Clone, Debug)]
pub struct ValidatedTarget {
    target: UrlTarget,
    addrs: Vec<SocketAddr>,
}

impl ValidatedTarget {
    /// The (host-normalized) URL to request.
    pub fn url(&self) -> &Url {
        &self.target.url
    }
    /// The canonical host (TLS SNI + the pin key).
    pub fn host(&self) -> &str {
        &self.target.host
    }
    /// The destination port.
    pub fn port(&self) -> u16 {
        self.target.port
    }
    /// The validated addresses to pin the connection to.
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }
}

/// Resolve a validated [`UrlTarget`] and pin it to its addresses. Split out from
/// [`guard`] so a caller (e.g. the fetch tool) can apply its allow/block policy
/// to the parsed target *before* spending a DNS lookup on a host it will refuse.
pub async fn resolve_and_pin<R: Resolve + ?Sized>(
    resolver: &R,
    target: UrlTarget,
) -> NetResult<ValidatedTarget> {
    let addrs = if let Some(ip) = target.literal_ip {
        vec![SocketAddr::new(ip, target.port)]
    } else {
        let ips = resolver.lookup(&target.host).await?;
        if ips.is_empty() {
            return Err(NetError::not_accessible(format!(
                "host `{}` did not resolve",
                target.host
            )));
        }
        // Validate EVERY resolved address; pin only if all pass. This both blocks
        // a poisoned record set and neutralizes happy-eyeballs A/AAAA races. The
        // refusal does not echo the resolved IP (it would be an internal-network
        // reconnaissance oracle across the tool boundary).
        for ip in &ips {
            if ip_is_blocked(*ip) {
                return Err(NetError::not_allowed(format!(
                    "host `{}` resolved to a blocked address",
                    target.host
                )));
            }
        }
        ips.into_iter()
            .map(|ip| SocketAddr::new(ip, target.port))
            .collect()
    };

    Ok(ValidatedTarget { target, addrs })
}

/// The guard entrypoint: resolve → validate → pin. Reused by fetch, download,
/// and (Phase 3) every redirect hop.
pub async fn guard<R: Resolve + ?Sized>(resolver: &R, raw: &str) -> NetResult<ValidatedTarget> {
    resolve_and_pin(resolver, validate_url(raw)?).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::error::NetErrorCode;
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_ipv4_ranges() {
        for addr in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // RFC1918
            "172.16.5.4",      // RFC1918
            "192.168.1.1",     // RFC1918
            "169.254.169.254", // AWS/GCP/Azure metadata
            "169.254.170.2",   // AWS ECS credentials endpoint
            "100.64.0.1",      // CGNAT
            "100.100.100.200", // Alibaba metadata (in CGNAT)
            "192.0.0.192",     // Oracle metadata
            "192.0.2.5",       // TEST-NET-1
            "198.18.0.1",      // benchmarking
            "192.88.99.1",     // 6to4 relay anycast
            "224.0.0.1",       // multicast
            "240.0.0.1",       // reserved
            "0.0.0.0",         // unspecified
        ] {
            assert!(ip_is_blocked(ip(addr)), "{addr} should be blocked");
        }
    }

    #[test]
    fn allows_public_ipv4() {
        for addr in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            assert!(!ip_is_blocked(ip(addr)), "{addr} should be allowed");
        }
    }

    #[test]
    fn blocks_ipv6_ranges_including_transition_forms() {
        for addr in [
            "::1",                    // loopback
            "::",                     // unspecified
            "100::1",                 // discard-only
            "fc00::1",                // ULA
            "fd00:ec2::254",          // AWS IMDS over IPv6 (in ULA)
            "fe80::1",                // link-local
            "fec0::1",                // site-local (deprecated)
            "64:ff9b::7f00:1",        // NAT64 → 127.0.0.1
            "64:ff9b:1::a9fe:a9fe",   // NAT64 local-use → 169.254.169.254
            "2002:7f00:1::",          // 6to4 embedding 127.0.0.1
            "2001::1",                // Teredo
            "2001:20::1",             // ORCHIDv2
            "5f00::1",                // SRv6
            "ff02::1",                // multicast
            "::ffff:127.0.0.1",       // IPv4-mapped loopback
            "::ffff:169.254.169.254", // IPv4-mapped metadata
        ] {
            assert!(ip_is_blocked(ip(addr)), "{addr} should be blocked");
        }
    }

    #[test]
    fn allows_public_ipv6_and_mapped_public() {
        assert!(!ip_is_blocked(ip("2606:4700:4700::1111"))); // Cloudflare
        assert!(!ip_is_blocked(ip("::ffff:8.8.8.8"))); // mapped public
    }

    #[test]
    fn normalize_host_folds_case_idna_and_trailing_dot() {
        assert_eq!(normalize_host("EXAMPLE.COM."), "example.com");
        assert_eq!(normalize_host("bücher.example"), "xn--bcher-kva.example");
    }

    #[test]
    fn validate_url_rejects_structural_problems() {
        assert_eq!(
            validate_url(&format!("https://example.com/{}", "a".repeat(300)))
                .unwrap_err()
                .code,
            NetErrorCode::UrlTooLong
        );
        assert_eq!(
            validate_url("ftp://example.com/").unwrap_err().code,
            NetErrorCode::InvalidUrl
        );
        assert_eq!(
            validate_url("https://user:pass@example.com/")
                .unwrap_err()
                .code,
            NetErrorCode::InvalidUrl
        );
        assert!(validate_url("not a url").is_err());
    }

    #[test]
    fn validate_url_blocks_ip_literals_and_encodings() {
        // Direct private/metadata literals.
        assert!(validate_url("http://127.0.0.1/").is_err());
        assert!(validate_url("http://169.254.169.254/").is_err());
        assert!(validate_url("http://[::1]/").is_err());
        // Encoded IPv4 literals: the WHATWG parser either rejects them or
        // normalizes them to the real IP, which the deny-list then catches.
        assert!(validate_url("http://0177.0.0.1/").is_err());
        assert!(validate_url("http://2130706433/").is_err());
        assert!(validate_url("http://0x7f.0.0.1/").is_err());
    }

    #[test]
    fn validate_url_blocks_internal_names() {
        for raw in [
            "http://localhost/",
            "http://foo.localhost/",
            "http://service.internal/",
            "http://metadata.google.internal/",
            "http://app.local/",
            "http://vault.corp/",
            "http://kubernetes.default.svc/",
        ] {
            assert_eq!(
                validate_url(raw).unwrap_err().code,
                NetErrorCode::UrlNotAllowed,
                "{raw} should be blocked"
            );
        }
    }

    #[test]
    fn validate_url_accepts_public_domain() {
        let target = validate_url("https://docs.example.com/page").unwrap();
        assert_eq!(target.host, "docs.example.com");
        assert_eq!(target.port, 443);
        assert!(target.literal_ip.is_none());
    }

    #[test]
    fn validate_url_normalizes_trailing_dot_host_for_pinning() {
        // host and url.host_str() must agree, or the Phase-2 pin won't bind and
        // the client re-resolves (reopening the rebinding window).
        let target = validate_url("https://example.com./page").unwrap();
        assert_eq!(target.host, "example.com");
        assert_eq!(target.url.host_str(), Some("example.com"));
    }

    // ---- guard (resolve → validate → pin) ----

    struct MockResolver {
        addrs: Vec<IpAddr>,
        calls: Arc<AtomicUsize>,
    }

    impl MockResolver {
        fn new(addrs: &[&str]) -> Self {
            Self {
                addrs: addrs.iter().map(|a| ip(a)).collect(),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Resolve for MockResolver {
        fn lookup(&self, _host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let addrs = self.addrs.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    #[tokio::test]
    async fn guard_pins_validated_public_address() {
        let resolver = MockResolver::new(&["93.184.216.34"]);
        let target = guard(&resolver, "https://example.com/").await.unwrap();
        assert_eq!(target.addrs(), [SocketAddr::new(ip("93.184.216.34"), 443)]);
    }

    #[tokio::test]
    async fn guard_rejects_when_any_resolved_ip_is_blocked() {
        // DNS rebinding / poisoned record set: one public, one internal.
        let resolver = MockResolver::new(&["93.184.216.34", "169.254.169.254"]);
        let err = guard(&resolver, "https://example.com/").await.unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAllowed);
        // The refusal must not leak the resolved internal IP.
        assert!(!err.message.contains("169.254.169.254"));
    }

    #[tokio::test]
    async fn guard_resolves_exactly_once() {
        let resolver = MockResolver::new(&["1.1.1.1"]);
        let calls = resolver.calls.clone();
        guard(&resolver, "https://example.com/").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn guard_rejects_unresolvable_host() {
        let resolver = MockResolver::new(&[]);
        let err = guard(&resolver, "https://example.com/").await.unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAccessible);
    }

    #[tokio::test]
    async fn guard_skips_dns_for_ip_literals() {
        let resolver = MockResolver::new(&["169.254.169.254"]); // would be blocked if used
        let target = guard(&resolver, "https://93.184.216.34/").await.unwrap();
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
        assert_eq!(target.addrs(), [SocketAddr::new(ip("93.184.216.34"), 443)]);
    }
}
