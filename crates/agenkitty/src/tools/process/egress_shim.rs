//! In-namespace egress relay shim (Tier-2 bypass-prevention).
//!
//! Invoked as the bubblewrap `--unshare-net` entrypoint via the hidden
//! `agenkitty __egress-shim` subcommand. Inside that namespace there is **no
//! internet route**; the host egress proxy's Unix socket is bound in. This shim
//! listens on loopback, splices each connection to that UDS (→ the allowlisted
//! proxy), sets `HTTP_PROXY` to its own loopback address, and runs the real
//! command — so the *only* reachable egress is the proxy, no matter how the child
//! behaves.
//!
//! The relay + supervisor are unit-tested on the host; the full namespace
//! integration (bwrap `--unshare-net` + bound UDS) is validated end-to-end by
//! `tests/process_tool.rs::egress_shim_is_the_only_route_out_of_the_namespace`
//! on a real Linux+bubblewrap host — direct egress is dead, proxied egress
//! round-trips through the shim → UDS → proxy. That validation caught (and
//! fixed) a real bug: `bwrap --tmpfs` on `/var/run` (a symlink to `/run` on
//! modern Linux) aborted the whole sandbox, so the mask now skips symlinked
//! dirs. The bwrap-gated tests skip *loudly* where namespaces are unavailable.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, UnixStream};

const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Run `command` behind a loopback→UDS relay pointing at `uds`. Returns the
/// command's exit code (or `1` on a shim setup failure). This is the process
/// image of the bwrap entrypoint.
pub async fn run_egress_shim(uds: PathBuf, command: Vec<String>) -> i32 {
    let Some((program, args)) = command.split_first() else {
        eprintln!("egress-shim: empty command");
        return 1;
    };

    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("egress-shim: loopback bind failed: {err}");
            return 1;
        }
    };
    let proxy = match listener.local_addr() {
        Ok(addr) => format!("http://{addr}"),
        Err(err) => {
            eprintln!("egress-shim: loopback addr failed: {err}");
            return 1;
        }
    };
    let relay = tokio::spawn(relay_loop(listener, uds));

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for key in PROXY_ENV_KEYS {
        cmd.env(key, &proxy);
    }

    let outcome = match cmd.spawn() {
        Ok(mut child) => child.wait().await,
        Err(err) => {
            eprintln!("egress-shim: spawn `{program}` failed: {err}");
            relay.abort();
            return 1;
        }
    };
    relay.abort();
    match outcome {
        Ok(status) => exit_code_preserving_signal(status),
        Err(err) => {
            eprintln!("egress-shim: wait failed: {err}");
            1
        }
    }
}

/// The child's exit code; if it was killed by a signal, re-raise that signal so
/// the shim's own termination mirrors it — otherwise the outer process tool would
/// report a bare `exit 1` and lose the signal. Falls back to `128 + signo`.
fn exit_code_preserving_signal(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        return code;
    }
    if let Some(raw) = status.signal() {
        if let Ok(sig) = nix::sys::signal::Signal::try_from(raw) {
            // SAFETY: the shim exists only to mirror the child's termination —
            // restore the default disposition and re-raise so we die by `sig`.
            unsafe {
                let _ = nix::sys::signal::signal(sig, nix::sys::signal::SigHandler::SigDfl);
            }
            let _ = nix::sys::signal::raise(sig);
        }
        return 128 + raw;
    }
    1
}

/// Accept loopback connections and splice each to a fresh proxy-UDS connection.
async fn relay_loop(listener: TcpListener, uds: PathBuf) {
    loop {
        let Ok((mut client, _)) = listener.accept().await else {
            continue;
        };
        let uds = uds.clone();
        tokio::spawn(async move {
            if let Ok(mut upstream) = UnixStream::connect(&uds).await {
                let _ = copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UnixListener};

    use super::*;

    #[tokio::test]
    async fn relay_splices_loopback_tcp_to_uds() {
        let dir = tempfile::tempdir().unwrap();
        let uds_path = dir.path().join("proxy.sock");
        // Fake proxy: echo back whatever it receives once.
        let uds = UnixListener::bind(&uds_path).unwrap();
        tokio::spawn(async move {
            if let Ok((mut server, _)) = uds.accept().await {
                let mut buf = [0u8; 8];
                let n = server.read(&mut buf).await.unwrap();
                server.write_all(&buf[..n]).await.unwrap();
            }
        });

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relay = tokio::spawn(relay_loop(listener, uds_path));

        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 8];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
        relay.abort();
    }

    #[tokio::test]
    async fn shim_sets_proxy_env_and_propagates_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let uds = dir.path().join("proxy.sock"); // need not exist; relay is lazy
        let code = run_egress_shim(
            uds,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "test -n \"$HTTP_PROXY\" && exit 7".to_string(),
            ],
        )
        .await;
        assert_eq!(code, 7, "child should see HTTP_PROXY and exit 7");
    }

    #[tokio::test]
    async fn shim_rejects_empty_command() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_egress_shim(dir.path().join("p.sock"), vec![]).await, 1);
    }
}
