//! Egress proxy — a host-side HTTP **CONNECT** proxy that gives the process
//! tool's subprocesses (cargo / npm / pip / git / curl) *allowlisted* outbound
//! network, instead of the sandbox's binary network on/off.
//!
//! It authorizes every CONNECT against the **same** [`NetPolicy`] +
//! [`ssrf::guard`] as `net.fetch`, so there is one egress policy: a tunnel is
//! opened only to an allowlisted host on an allowed port whose resolved address
//! passed the SSRF deny-list, and the connection is made to that **pinned**
//! address. TLS is tunnelled opaquely (hostname/SNI only — no interception),
//! matching Claude Code's model.
//!
//! A host wires it by [`bind_local`](EgressProxy::bind_local)-ing the proxy and
//! injecting `HTTP_PROXY`/`HTTPS_PROXY=http://<addr>` into the sandboxed child's
//! environment. This works for well-behaved HTTP clients today; preventing a
//! determined bypass (so the *only* egress is the proxy) is the namespace /
//! firewall hardening tracked as the next step in the README roadmap.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
#[cfg(unix)]
use {std::path::Path, tokio::net::UnixListener};

use super::error::{NetError, NetErrorCode, NetResult};
use super::fetch::HickoryResolver;
use super::policy::NetPolicy;
use super::ssrf::{Resolve, ValidatedTarget, guard};

/// Cap on the CONNECT request head we will buffer before giving up.
const MAX_HEAD_BYTES: usize = 8 * 1024;
/// How long to wait for the client's CONNECT request line.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-address dial timeout when establishing the upstream tunnel.
const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// A CONNECT forward proxy bound to one [`NetPolicy`] + SSRF guard.
#[derive(Clone)]
pub struct EgressProxy {
    policy: NetPolicy,
    resolver: Arc<dyn Resolve>,
    /// Tunnels opened by this proxy, for the policy's `max_requests` budget.
    requests: Arc<AtomicUsize>,
}

impl EgressProxy {
    /// Build with the production hickory resolver.
    pub fn new(policy: NetPolicy) -> NetResult<Self> {
        Ok(Self {
            policy,
            resolver: Arc::new(HickoryResolver::new()?),
            requests: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Build with an injected resolver (tests).
    pub fn with_resolver(policy: NetPolicy, resolver: Arc<dyn Resolve>) -> Self {
        Self {
            policy,
            resolver,
            requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Count this tunnel against the policy's request budget, if any.
    fn charge_request(&self) -> NetResult<()> {
        if let Some(max) = self.policy.max_requests()
            && self.requests.fetch_add(1, Ordering::SeqCst) >= max
        {
            return Err(NetError::too_many_requests(format!(
                "egress request budget of {max} exhausted"
            )));
        }
        Ok(())
    }

    /// Bind a listener on `127.0.0.1:0`, serve in a background task, and return
    /// the bound address (for `HTTP_PROXY=http://<addr>`) plus the task handle.
    pub async fn bind_local(self) -> NetResult<(SocketAddr, JoinHandle<()>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|err| NetError::not_accessible(format!("proxy bind failed: {err}")))?;
        let addr = listener
            .local_addr()
            .map_err(|err| NetError::not_accessible(format!("proxy addr failed: {err}")))?;
        let handle = tokio::spawn(async move {
            let _ = self.serve(listener).await;
        });
        Ok((addr, handle))
    }

    /// Accept TCP connections forever, handling each as a CONNECT tunnel.
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (client, _peer) = listener.accept().await?;
            let proxy = self.clone();
            tokio::spawn(async move {
                let _ = proxy.handle_connection(client).await;
            });
        }
    }

    /// Bind a Unix-socket listener and serve in a background task. A UDS is the
    /// transport that crosses a network namespace, so this is how a child in
    /// `--unshare-net` (no internet route) reaches the proxy: bind the socket
    /// into the sandbox and point a loopback relay at it.
    #[cfg(unix)]
    pub async fn bind_uds(self, path: impl AsRef<Path>) -> NetResult<JoinHandle<()>> {
        let listener = UnixListener::bind(path.as_ref())
            .map_err(|err| NetError::not_accessible(format!("proxy UDS bind failed: {err}")))?;
        Ok(tokio::spawn(async move {
            let _ = self.serve_uds(listener).await;
        }))
    }

    /// Accept Unix-socket connections forever, handling each as a CONNECT tunnel.
    #[cfg(unix)]
    pub async fn serve_uds(self, listener: UnixListener) -> std::io::Result<()> {
        loop {
            let (client, _peer) = listener.accept().await?;
            let proxy = self.clone();
            tokio::spawn(async move {
                let _ = proxy.handle_connection(client).await;
            });
        }
    }

    /// Authorize a CONNECT target against the allowlist + scheme/port policy and
    /// the SSRF guard (resolve → validate → pin) — the same gate as `net.fetch`.
    async fn authorize_connect(&self, host: &str, port: u16) -> NetResult<ValidatedTarget> {
        // A CONNECT tunnel carries TLS (https); require https to be permitted, so
        // an http-only policy doesn't get an opaque tunnel anyway.
        if !self.policy.scheme_allowed("https") {
            return Err(NetError::not_allowed(
                "CONNECT (https) is not permitted by policy",
            ));
        }
        if !self.policy.host_allowed(host) {
            return Err(NetError::not_allowed(format!(
                "host `{host}` is not in the allowlist"
            )));
        }
        if !self.policy.port_allowed(port) {
            return Err(NetError::not_allowed(format!(
                "port {port} is not allowed by policy"
            )));
        }
        self.charge_request()?;
        guard(self.resolver.as_ref(), &connect_url(host, port)).await
    }

    async fn handle_connection<S>(&self, mut client: S) -> std::io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // Require a complete `\r\n\r\n`-terminated head within the cap; otherwise
        // the advertised limit could be bypassed by trailing bytes that would
        // become tunnel data.
        let head = match tokio::time::timeout(HEAD_TIMEOUT, read_request_head(&mut client)).await {
            Ok(Ok(Some(head))) => head,
            _ => return write_status(&mut client, "400 Bad Request").await,
        };
        let (host, port) = match parse_connect_target(&head) {
            Ok(target) => target,
            Err(()) => return write_status(&mut client, "400 Bad Request").await,
        };

        let target = match self.authorize_connect(&host, port).await {
            Ok(target) => target,
            Err(err) => {
                tracing::warn!(
                    target: "pocopine.log",
                    proxy = "egress",
                    host = %host,
                    port,
                    reason = err.code.as_str(),
                    "CONNECT denied"
                );
                let status = match err.code {
                    NetErrorCode::TooManyRequests => "429 Too Many Requests",
                    _ => "403 Forbidden",
                };
                return write_status(&mut client, status).await;
            }
        };

        // Try every validated address (dual-stack hosts) before giving up — they
        // all passed the deny-list, so any is a safe pin.
        let mut upstream = None;
        for addr in target.addrs() {
            if let Ok(Ok(stream)) =
                tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(*addr)).await
            {
                upstream = Some(stream);
                break;
            }
        }
        let Some(mut upstream) = upstream else {
            return write_status(&mut client, "502 Bad Gateway").await;
        };

        tracing::info!(
            target: "pocopine.log",
            proxy = "egress",
            host = %host,
            port,
            "CONNECT established"
        );
        write_status(&mut client, "200 Connection established").await?;
        // Opaque bidirectional tunnel — the child does its own TLS end-to-end.
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        Ok(())
    }
}

/// Build the `https://host:port/` URL the SSRF guard validates for a CONNECT
/// target (bracketing IPv6 literals).
fn connect_url(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("https://[{host}]:{port}/")
    } else {
        format!("https://{host}:{port}/")
    }
}

/// Parse the `host`/`port` from a proxy CONNECT request head. Accepts only the
/// `CONNECT host:port HTTP/1.1` form; IPv6 literals are unbracketed.
fn parse_connect_target(head: &str) -> Result<(String, u16), ()> {
    let first_line = head.lines().next().ok_or(())?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or(())?;
    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(());
    }
    let authority = parts.next().ok_or(())?;
    let (host, port) = authority.rsplit_once(':').ok_or(())?;
    let port: u16 = port.parse().map_err(|_| ())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(());
    }
    Ok((host.to_string(), port))
}

/// Read the request head up to (and including) the terminating `\r\n\r\n`, one
/// byte at a time so we never consume bytes belonging to the tunnelled stream.
/// Returns `None` if the cap is hit or the peer closes before the terminator —
/// an incomplete head must not be treated as a valid request.
async fn read_request_head<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> std::io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < MAX_HEAD_BYTES {
        if stream.read(&mut byte).await? == 0 {
            return Ok(None);
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
    }
    Ok(None)
}

async fn write_status<S: AsyncWrite + Unpin>(stream: &mut S, status: &str) -> std::io::Result<()> {
    stream
        .write_all(format!("HTTP/1.1 {status}\r\n\r\n").as_bytes())
        .await
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use pocopine_agenkit::server::BoxFuture;

    use super::super::error::NetErrorCode;
    use super::*;

    struct MockResolver(Vec<IpAddr>);
    impl Resolve for MockResolver {
        fn lookup(&self, _host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    fn proxy(policy: NetPolicy, resolved: &[&str]) -> EgressProxy {
        let addrs = resolved.iter().map(|a| a.parse().unwrap()).collect();
        EgressProxy::with_resolver(policy, Arc::new(MockResolver(addrs)))
    }

    #[test]
    fn parses_connect_targets() {
        assert_eq!(
            parse_connect_target("CONNECT example.com:443 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap(),
            ("example.com".to_string(), 443)
        );
        assert_eq!(
            parse_connect_target("CONNECT [2606:4700::1111]:8443 HTTP/1.1\r\n\r\n").unwrap(),
            ("2606:4700::1111".to_string(), 8443)
        );
        // Non-CONNECT method and missing port are rejected.
        assert!(parse_connect_target("GET http://example.com/ HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_connect_target("CONNECT example.com HTTP/1.1\r\n\r\n").is_err());
    }

    #[tokio::test]
    async fn authorize_connect_enforces_policy_and_ssrf() {
        let p = proxy(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        assert!(p.authorize_connect("example.com", 443).await.is_ok());
        // not allowlisted
        assert_eq!(
            p.authorize_connect("evil.com", 443).await.unwrap_err().code,
            NetErrorCode::UrlNotAllowed
        );
        // port not permitted (default 443 only)
        assert_eq!(
            p.authorize_connect("example.com", 8080)
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_connect_blocks_ssrf_for_allowlisted_host() {
        let p = proxy(NetPolicy::allow(["example.com"]), &["169.254.169.254"]);
        assert_eq!(
            p.authorize_connect("example.com", 443)
                .await
                .unwrap_err()
                .code,
            NetErrorCode::UrlNotAllowed
        );
    }

    #[tokio::test]
    async fn authorize_connect_charges_request_budget() {
        let p = proxy(
            NetPolicy::allow(["example.com"]).with_max_requests(1),
            &["93.184.216.34"],
        );
        assert!(p.authorize_connect("example.com", 443).await.is_ok());
        assert_eq!(
            p.authorize_connect("example.com", 443)
                .await
                .unwrap_err()
                .code,
            NetErrorCode::TooManyRequests
        );
    }

    #[tokio::test]
    async fn connect_to_disallowed_host_gets_403_end_to_end() {
        let p = proxy(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        let (addr, _handle) = p.bind_local().await.unwrap();

        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"CONNECT evil.com:443 HTTP/1.1\r\nHost: evil.com:443\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 64];
        let n = client.read(&mut resp).await.unwrap();
        let resp = String::from_utf8_lossy(&resp[..n]);
        assert!(resp.contains("403"), "expected 403, got: {resp}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn connect_over_uds_to_disallowed_host_gets_403() {
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("egress.sock");
        let p = proxy(NetPolicy::allow(["example.com"]), &["93.184.216.34"]);
        let _handle = p.bind_uds(&sock).await.unwrap();

        let mut client = UnixStream::connect(&sock).await.unwrap();
        client
            .write_all(b"CONNECT evil.com:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 64];
        let n = client.read(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp[..n]).contains("403"));
    }
}
