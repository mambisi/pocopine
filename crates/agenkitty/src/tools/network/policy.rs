//! Network policy: the host allow/block-list and scheme/port restrictions that
//! gate which destinations a fetch may reach. This is the *allowlist* layer; the
//! unconditional SSRF deny-list (private / metadata / internal targets) lives in
//! [`super::ssrf`] and always applies regardless of policy.

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use super::ssrf::normalize_host;

/// Per-policy domain allow/block configuration plus scheme/port restrictions.
///
/// `allowed_domains` and `blocked_domains` are mutually exclusive. Host matching
/// is canonicalized through [`normalize_host`] (the same helper the SSRF layer
/// uses, so both layers agree on host identity — lowercase, IDNA/punycode,
/// trailing-dot-stripped) and uses **label-boundary suffix** matching (never
/// substring): `example.com` matches `example.com` and `api.example.com`, but
/// not `evilexample.com`. With no allowlist set, every host is denied. Defaults
/// are https-only on port 443.
/// Default streamed response cap (5 MB). Applies to the *decompressed* body, so
/// it doubles as the decompression-bomb ceiling.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct NetPolicy {
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    allowed_schemes: Vec<String>,
    allowed_ports: Vec<u16>,
    max_response_bytes: usize,
    max_requests: Option<usize>,
}

impl Default for NetPolicy {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            allowed_schemes: vec!["https".to_string()],
            allowed_ports: vec![443],
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_requests: None,
        }
    }
}

impl NetPolicy {
    /// Build a policy, rejecting the mutually-exclusive misconfiguration.
    pub fn new(allowed_domains: Vec<String>, blocked_domains: Vec<String>) -> AgenkitResult<Self> {
        if !allowed_domains.is_empty() && !blocked_domains.is_empty() {
            return Err(AgenkitError::config(
                "net policy: `allowed_domains` and `blocked_domains` are mutually exclusive",
            ));
        }
        Ok(Self {
            allowed_domains: normalize_domains(allowed_domains),
            blocked_domains: normalize_domains(blocked_domains),
            ..Self::default()
        })
    }

    /// An allowlist-only policy (the common case: opt in a set of doc domains).
    pub fn allow<I, S>(domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed_domains: normalize_domains(domains.into_iter().map(Into::into).collect()),
            ..Self::default()
        }
    }

    /// Override the allowed URL schemes (default: `https`).
    pub fn with_schemes<I, S>(mut self, schemes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_schemes = schemes
            .into_iter()
            .map(|s| s.into().to_ascii_lowercase())
            .collect();
        self
    }

    /// Override the allowed ports (default: `443`).
    pub fn with_ports<I: IntoIterator<Item = u16>>(mut self, ports: I) -> Self {
        self.allowed_ports = ports.into_iter().collect();
        self
    }

    /// Override the decompressed response-body cap (default 5 MB).
    pub fn with_max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Cap the number of fetches this registration may perform (default: unlimited).
    pub fn with_max_requests(mut self, max: usize) -> Self {
        self.max_requests = Some(max);
        self
    }

    /// The decompressed response-body cap.
    pub fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// The request-count budget, if any.
    pub fn max_requests(&self) -> Option<usize> {
        self.max_requests
    }

    /// Whether `host` is permitted by the allow/block configuration.
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = normalize_host(host);
        if !self.blocked_domains.is_empty() {
            return !self.blocked_domains.iter().any(|d| host_matches(&host, d));
        }
        if self.allowed_domains.is_empty() {
            return false;
        }
        self.allowed_domains.iter().any(|d| host_matches(&host, d))
    }

    /// Whether `scheme` is permitted (case-insensitive).
    pub fn scheme_allowed(&self, scheme: &str) -> bool {
        self.allowed_schemes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(scheme))
    }

    /// Whether `port` is permitted.
    pub fn port_allowed(&self, port: u16) -> bool {
        self.allowed_ports.contains(&port)
    }
}

fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    domains
        .iter()
        .map(|d| normalize_host(d))
        .filter(|d| !d.is_empty())
        .collect()
}

/// Label-boundary suffix (or exact) match, allocation-free. `host` and `domain`
/// are pre-normalized.
fn host_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.strip_suffix(domain)
        .and_then(|prefix| prefix.strip_suffix('.'))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_and_blocklist_are_mutually_exclusive() {
        let err = NetPolicy::new(vec!["a.com".into()], vec!["b.com".into()]).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn empty_policy_denies_every_host() {
        let policy = NetPolicy::default();
        assert!(!policy.host_allowed("example.com"));
    }

    #[test]
    fn allowlist_matches_exact_and_subdomain_at_label_boundary() {
        let policy = NetPolicy::allow(["example.com"]);
        assert!(policy.host_allowed("example.com"));
        assert!(policy.host_allowed("api.example.com"));
        assert!(policy.host_allowed("EXAMPLE.com")); // case-insensitive
        assert!(policy.host_allowed("example.com.")); // trailing dot
        // The classic substring bypass must NOT match.
        assert!(!policy.host_allowed("evilexample.com"));
        assert!(!policy.host_allowed("example.com.evil.com"));
        assert!(!policy.host_allowed("other.com"));
    }

    #[test]
    fn allowlist_canonicalizes_idn_to_punycode() {
        // Operator allowlists the Unicode form; a fetch resolves the host through
        // the url crate to its punycode form — both must agree.
        let policy = NetPolicy::allow(["bücher.example"]);
        assert!(policy.host_allowed("xn--bcher-kva.example"));
        assert!(policy.host_allowed("bücher.example"));
    }

    #[test]
    fn blocklist_denies_listed_and_allows_the_rest() {
        let policy = NetPolicy::new(vec![], vec!["blocked.com".into()]).unwrap();
        assert!(!policy.host_allowed("blocked.com"));
        assert!(!policy.host_allowed("sub.blocked.com"));
        assert!(policy.host_allowed("allowed.com"));
    }

    #[test]
    fn scheme_and_port_defaults_are_https_443() {
        let policy = NetPolicy::default();
        assert!(policy.scheme_allowed("https"));
        assert!(policy.scheme_allowed("HTTPS"));
        assert!(!policy.scheme_allowed("http"));
        assert!(policy.port_allowed(443));
        assert!(!policy.port_allowed(8080));
        assert!(!policy.port_allowed(25));
    }

    #[test]
    fn scheme_and_port_overrides_apply() {
        let policy = NetPolicy::default()
            .with_schemes(["http", "https"])
            .with_ports([80, 443, 8443]);
        assert!(policy.scheme_allowed("http"));
        assert!(policy.port_allowed(8443));
        assert!(!policy.port_allowed(22));
    }
}
