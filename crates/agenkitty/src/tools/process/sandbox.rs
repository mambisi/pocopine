//! Per-child kernel sandboxing for spawned processes.
//!
//! On Linux: **Landlock** confines filesystem writes to the declared roots (the
//! rest of `/` stays readable + executable, but not writable), and a
//! **seccomp-bpf** filter denies `AF_INET`/`AF_INET6` socket creation when
//! network is off (`AF_UNIX`/`AF_NETLINK` still work). Both are *built in the
//! parent* — where allocation is safe — and only **applied** (syscalls only) in
//! the child's `pre_exec` hook, so we never allocate in the post-fork child.
//!
//! On non-Linux unix targets this is a no-op (use the `SandboxHost` seam /
//! Seatbelt there); the rest of the process tool still applies its portable
//! protections (cwd confinement, scrubbed env, rlimits, group-kill).

use std::path::{Path, PathBuf};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

/// How the policy is enforced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxBackend {
    /// In-process kernel confinement: Landlock (filesystem) + seccomp (syscalls).
    /// Zero startup cost, edits the workspace in place, but shares the host
    /// kernel and gives coarse (all-or-nothing INET) network control.
    #[default]
    InProcess,
    /// Wrap the command in **bubblewrap** (`bwrap`): mount/pid/net/user
    /// namespaces. Stronger isolation (its own process table, a fresh network
    /// namespace) at the cost of spawning `bwrap`. The filesystem rules map to
    /// `--ro-bind / /` + `--bind <writable_root>`; network-off maps to
    /// `--unshare-net`.
    Bubblewrap,
}

/// What a spawned process is allowed to touch, and how that's enforced.
#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    /// Confine filesystem writes to `writable_roots`; the rest of `/` is
    /// read-only (still readable + executable, just not writable).
    pub confine_filesystem: bool,
    /// Absolute paths the child may write under (when `confine_filesystem`).
    pub writable_roots: Vec<PathBuf>,
    /// Allow `AF_INET`/`AF_INET6` sockets. When false they fail with `EPERM`.
    pub allow_network: bool,
    /// Tier-2 egress lockdown (bubblewrap only): the child runs in `--unshare-net`
    /// (no internet route) with this Unix socket — the host egress proxy — bound
    /// in, and is wrapped by an in-namespace loopback→UDS relay shim. Loopback
    /// INET stays allowed (the relay needs it); the *namespace* is the egress
    /// control. `None` = no proxy lockdown.
    pub egress_proxy_uds: Option<PathBuf>,
    /// Binary providing the `__egress-shim` entrypoint (Tier-2). `None` uses
    /// `current_exe()` — correct when the running binary is the agenkitty CLI; a
    /// downstream host that embeds agenkitty as a library must point this at an
    /// agenkitty binary.
    pub egress_shim_path: Option<PathBuf>,
    /// Enforcement mechanism.
    pub backend: SandboxBackend,
}

impl SandboxPolicy {
    /// Confine writes to the workspace root plus `/tmp`, and deny network — the
    /// default for the process tools (mirrors `SandboxSpec { network: "none" }`).
    pub fn workspace(root: impl Into<PathBuf>) -> Self {
        Self {
            confine_filesystem: true,
            writable_roots: vec![root.into(), PathBuf::from("/tmp")],
            allow_network: false,
            egress_proxy_uds: None,
            egress_shim_path: None,
            backend: SandboxBackend::InProcess,
        }
    }

    /// Confine writes to exactly `writable_roots`, deny network.
    pub fn confined(writable_roots: Vec<PathBuf>) -> Self {
        Self {
            confine_filesystem: true,
            writable_roots,
            allow_network: false,
            egress_proxy_uds: None,
            egress_shim_path: None,
            backend: SandboxBackend::InProcess,
        }
    }

    /// No filesystem confinement, network allowed.
    pub fn unconfined() -> Self {
        Self {
            confine_filesystem: false,
            writable_roots: Vec::new(),
            allow_network: true,
            egress_proxy_uds: None,
            egress_shim_path: None,
            backend: SandboxBackend::InProcess,
        }
    }

    /// Allow network while keeping any filesystem confinement.
    pub fn with_network(mut self) -> Self {
        self.allow_network = true;
        self
    }

    /// Enforce via bubblewrap namespaces instead of in-process Landlock+seccomp.
    pub fn using_bubblewrap(mut self) -> Self {
        self.backend = SandboxBackend::Bubblewrap;
        self
    }

    /// Lock egress to a host proxy reachable only via `uds` (Tier-2). Requires
    /// the bubblewrap backend (it relies on `--unshare-net`); the caller should
    /// also set [`using_bubblewrap`](Self::using_bubblewrap).
    pub fn with_egress_proxy_uds(mut self, uds: impl Into<PathBuf>) -> Self {
        self.egress_proxy_uds = Some(uds.into());
        self
    }

    /// Set the binary that provides the `__egress-shim` entrypoint (Tier-2).
    /// Defaults to `current_exe()`.
    pub fn with_egress_shim_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.egress_shim_path = Some(path.into());
        self
    }

    /// Whether seccomp should deny `AF_INET`/`AF_INET6`. Network-off denies INET,
    /// **except** in egress-proxy mode, where the namespace provides isolation and
    /// loopback INET must stay open for the relay.
    pub fn deny_inet(&self) -> bool {
        !self.allow_network && self.egress_proxy_uds.is_none()
    }
}

/// The trusted absolute path to `bwrap`. We never resolve `bwrap` via `PATH`,
/// since the (untrusted) request environment could otherwise redirect the
/// *unsandboxed* wrapper to an attacker-planted binary.
pub(crate) fn trusted_bwrap_path() -> AgenkitResult<std::path::PathBuf> {
    for candidate in ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"] {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Ok(path.to_path_buf());
        }
    }
    Err(AgenkitError::config(
        "bubblewrap (bwrap) not found in /usr/bin, /bin, or /usr/local/bin",
    ))
}

/// Build the `bwrap` arguments (everything up to and including `--`) for a
/// policy: a read-only `/`, read-write binds for each existing writable root, a
/// fresh `/dev` and `/proc`, an isolated pid namespace (so the child can't see
/// or signal host processes), an isolated network namespace when network is off,
/// `--die-with-parent`, a new session, the working directory, and `--setenv` for
/// each caller-supplied env var (set on the **sandboxed child**, not the trusted
/// wrapper process). The caller appends the real program + args after the `--`.
pub(crate) fn bwrap_args(
    policy: &SandboxPolicy,
    cwd: &Path,
    child_env: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let mut args = vec![
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--unshare-pid".to_string(),
        "--die-with-parent".to_string(),
        "--new-session".to_string(),
    ];
    // Egress mode isolates the network namespace (no internet route). With INET
    // routes gone, the remaining direct-egress vector is a host *pathname* AF_UNIX
    // socket (e.g. /run/docker.sock) reachable through the read-only `/` bind, so
    // overlay tmpfs on the common socket dirs to hide them. Writable roots are
    // bound after the masks so a /tmp-based workspace remains visible; exact
    // masked dirs such as /tmp stay as fresh tmpfs instead of re-exposing the
    // host directory. The proxy socket is bound last so it survives all masks and
    // writable-root binds. Abstract-namespace sockets are already unreachable —
    // they are scoped to the (unshared) net ns. Residual: pathname sockets outside
    // these dirs (or inside a bound writable root) are not masked; don't expose
    // sensitive sockets there under Tier-2.
    if let Some(uds) = &policy.egress_proxy_uds {
        args.push("--unshare-net".to_string());
        // Mask the socket dirs, but resolve each to its REAL directory first.
        // `/var/run` is a symlink to `/run` on modern Linux, and `bwrap --tmpfs`
        // on a symlink path aborts the whole sandbox ("Can't mount tmpfs on
        // …/var/run: No such file or directory"). Resolving to the canonical
        // target and masking THAT (a) avoids the abort and (b) can never leave a
        // symlinked mask dir's target exposed — the target is what gets the
        // tmpfs, regardless of how the entry aliases it. Aliases collapse to one
        // mount via the dedup.
        for real in real_mask_dirs() {
            args.push("--tmpfs".to_string());
            args.push(real);
        }
        bind_writable_roots(policy, &mut args, true);
        let path = uds.display().to_string();
        args.push("--bind".to_string());
        args.push(path.clone());
        args.push(path);
    } else if !policy.allow_network {
        args.push("--unshare-net".to_string());
        bind_writable_roots(policy, &mut args, false);
    } else {
        bind_writable_roots(policy, &mut args, false);
    }
    // `child_env` is applied to the *child* via --setenv (so it can't influence
    // the trusted wrapper, e.g. via LD_PRELOAD). It must already exclude any
    // secret keys — those ride the wrapper's (owner-only) inherited environment
    // and must win, and we never put secret values on bwrap's argv.
    for (key, value) in child_env {
        args.push("--setenv".to_string());
        args.push(key.clone());
        args.push(value.clone());
    }
    args.push("--chdir".to_string());
    args.push(cwd.display().to_string());
    args.push("--".to_string());
    args
}

const EGRESS_SOCKET_MASK_DIRS: &[&str] = &["/run", "/var/run", "/tmp", "/dev/shm"];

/// The canonical real directories to tmpfs-mask in egress mode: each entry of
/// [`EGRESS_SOCKET_MASK_DIRS`] resolved through symlinks to its real directory,
/// deduplicated, and dropping any that is missing or not a directory. Masking
/// the *canonical target* (not the alias) is what keeps `bwrap --tmpfs` off a
/// symlink (which would abort the sandbox) while guaranteeing the real socket
/// dir behind an alias like `/var/run` → `/run` is still hidden.
fn real_mask_dirs() -> Vec<String> {
    let mut real = Vec::new();
    for dir in EGRESS_SOCKET_MASK_DIRS {
        // `canonicalize` resolves symlinks and requires the path to exist.
        let Ok(canonical) = std::fs::canonicalize(dir) else {
            continue;
        };
        if !canonical.is_dir() {
            continue;
        }
        let canonical = canonical.display().to_string();
        if !real.contains(&canonical) {
            real.push(canonical);
        }
    }
    real
}

fn bind_writable_roots(policy: &SandboxPolicy, args: &mut Vec<String>, egress_mode: bool) {
    if policy.confine_filesystem {
        for root in &policy.writable_roots {
            if root.exists() && !(egress_mode && is_exact_egress_mask_dir(root)) {
                bind_path(args, root);
            }
        }
    } else {
        // No filesystem confinement: bind `/` read-write.
        args.push("--bind".to_string());
        args.push("/".to_string());
        args.push("/".to_string());
    }
}

fn is_exact_egress_mask_dir(path: &Path) -> bool {
    EGRESS_SOCKET_MASK_DIRS
        .iter()
        .any(|masked| path == Path::new(masked))
}

fn bind_path(args: &mut Vec<String>, path: &Path) {
    let path = path.display().to_string();
    args.push("--bind".to_string());
    args.push(path.clone());
    args.push(path);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn deny_inet_is_off_in_egress_mode() {
        // Network-off denies INET; with_network allows it.
        assert!(SandboxPolicy::workspace("/tmp/x").deny_inet());
        assert!(
            !SandboxPolicy::workspace("/tmp/x")
                .with_network()
                .deny_inet()
        );
        // Egress mode keeps loopback INET open (the namespace is the control).
        assert!(
            !SandboxPolicy::workspace("/tmp/x")
                .with_egress_proxy_uds("/run/egress.sock")
                .deny_inet()
        );
    }

    #[test]
    fn bwrap_args_egress_unshares_net_and_binds_the_uds() {
        let workspace = tempfile::tempdir().unwrap();
        let workspace_root = workspace.path().to_path_buf();
        let policy = SandboxPolicy::workspace(workspace_root.clone())
            .using_bubblewrap()
            .with_egress_proxy_uds("/run/egress.sock");
        let args = bwrap_args(&policy, &workspace_root, &BTreeMap::new());
        assert!(args.iter().any(|a| a == "--unshare-net"));
        // Host socket dirs are masked with tmpfs so AF_UNIX can't reach e.g.
        // /run/docker.sock directly. `/run` and `/tmp` are real dirs on any
        // host, so they are always masked.
        for dir in ["/run", "/tmp"] {
            assert!(
                args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == dir),
                "{dir} should be tmpfs-masked in egress mode"
            );
        }
        // Invariant: EVERY dir handed to `--tmpfs` must be a CANONICAL real
        // directory (equal to its own canonicalization → not a symlink).
        // `bwrap --tmpfs` on a symlink (e.g. `/var/run` → `/run` on modern
        // Linux) aborts the whole sandbox, so mask dirs are resolved to their
        // real targets — the regression F4 validation caught on a real host.
        for w in args.windows(2) {
            if w[0] == "--tmpfs" {
                let target = std::path::Path::new(&w[1]);
                assert!(target.is_dir(), "--tmpfs target `{}` must be a dir", w[1]);
                assert_eq!(
                    std::fs::canonicalize(target).ok().as_deref(),
                    Some(target),
                    "--tmpfs target `{}` must be canonical (not a symlink)",
                    w[1]
                );
            }
        }
        // The proxy UDS is bound AFTER the tmpfs masks so it survives them.
        let uds_bind = args.windows(3).position(|w| {
            w[0] == "--bind" && w[1] == "/run/egress.sock" && w[2] == "/run/egress.sock"
        });
        let run_tmpfs = args
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/run");
        let tmp_tmpfs = args
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/tmp")
            .expect("/tmp should be tmpfs-masked");
        let workspace_bind = bind_position(&args, workspace_root.to_str().unwrap())
            .expect("workspace root should remain bound after /tmp is masked");
        let host_tmp_bind = bind_position(&args, "/tmp");
        assert!(uds_bind.is_some(), "proxy UDS should be bound in");
        assert!(
            uds_bind > run_tmpfs,
            "UDS bind must come after the tmpfs mask"
        );
        assert!(
            workspace_bind > tmp_tmpfs,
            "/tmp-based workspace bind must survive the /tmp tmpfs mask"
        );
        assert!(
            host_tmp_bind.is_none(),
            "egress mode should keep exact /tmp as fresh tmpfs, not host-bind it"
        );
    }

    fn bind_position(args: &[String], path: &str) -> Option<usize> {
        args.windows(3)
            .position(|w| w[0] == "--bind" && w[1] == path && w[2] == path)
    }
}

#[cfg(target_os = "linux")]
pub use linux::SandboxInstaller;
#[cfg(not(target_os = "linux"))]
pub use other::SandboxInstaller;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use landlock::{
        ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreated, RulesetCreatedAttr,
    };
    use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };

    use super::SandboxPolicy;

    // Landlock ABI v3 (Truncate) is a good portable baseline; BestEffort
    // downgrades on older kernels and we deliberately stop short of v5's
    // IoctlDev so device ioctls (e.g. on /dev/null stdio) keep working.
    const LANDLOCK_ABI: ABI = ABI::V3;

    /// Sandbox rules pre-built in the parent, ready to enforce on the child.
    pub struct SandboxInstaller {
        landlock: Option<RulesetCreated>,
        seccomp: Option<BpfProgram>,
    }

    impl SandboxInstaller {
        /// Build the ruleset + filter in the parent (allocation happens here).
        pub fn build(policy: &SandboxPolicy) -> AgenkitResult<Self> {
            let landlock = if policy.confine_filesystem {
                build_landlock(&policy.writable_roots)?
            } else {
                None
            };
            // Build a seccomp filter whenever any confinement is active: it
            // always denies the ptrace/io_uring escape vectors, and additionally
            // denies INET sockets when network is off. A fully unconfined policy
            // installs no filter.
            let seccomp = if policy.confine_filesystem || policy.deny_inet() {
                build_seccomp(policy.deny_inet())?
            } else {
                None
            };
            Ok(Self { landlock, seccomp })
        }

        /// Just the seccomp filter (no Landlock) — for bubblewrap mode, where the
        /// namespaces do the filesystem/network isolation but we still want the
        /// ptrace/io_uring (and INET-when-off) denial. The filter is applied to
        /// the `bwrap` wrapper and inherited across its exec into the child; none
        /// of the denied syscalls are used by bwrap's namespace setup.
        pub fn seccomp_only(policy: &SandboxPolicy) -> AgenkitResult<Self> {
            Ok(Self {
                landlock: None,
                seccomp: build_seccomp(policy.deny_inet())?,
            })
        }

        /// Enforce on the current thread — called from the child's `pre_exec`.
        /// Only performs syscalls, so it is fork-safe: the failure paths map to a
        /// **fixed errno** (no `format!`, no boxing) so the post-fork child fails
        /// closed without allocating while another thread may hold the allocator
        /// lock. The parent surfaces the errno as the spawn error. Landlock first,
        /// seccomp last (after seccomp, further restriction syscalls may
        /// themselves be filtered).
        pub fn apply(&mut self) -> std::io::Result<()> {
            if let Some(ruleset) = self.landlock.take() {
                ruleset
                    .restrict_self()
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EACCES))?;
            }
            if let Some(program) = &self.seccomp {
                seccompiler::apply_filter(program)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))?;
            }
            Ok(())
        }
    }

    fn build_landlock(writable: &[PathBuf]) -> AgenkitResult<Option<RulesetCreated>> {
        let read_only = AccessFs::from_read(LANDLOCK_ABI);
        let read_write = AccessFs::from_read(LANDLOCK_ABI) | AccessFs::from_write(LANDLOCK_ABI);

        let root_fd = PathFd::new("/")
            .map_err(|err| AgenkitError::config(format!("landlock open `/`: {err}")))?;
        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(read_write)
            .map_err(map_landlock)?
            .create()
            .map_err(map_landlock)?
            .add_rule(PathBeneath::new(root_fd, read_only))
            .map_err(map_landlock)?;

        for root in writable {
            // A writable root that doesn't exist is simply skipped — confinement
            // stays correct (it just isn't granted).
            if let Ok(fd) = PathFd::new(root) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(fd, read_write))
                    .map_err(map_landlock)?;
            }
        }

        Ok(Some(ruleset))
    }

    fn build_seccomp(deny_inet: bool) -> AgenkitResult<Option<BpfProgram>> {
        let Some(arch) = target_arch() else {
            return Ok(None);
        };
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

        // Always deny debugging / async-IO escape vectors. An empty rule vector
        // matches the syscall unconditionally, so it always hits the Errno action.
        for syscall in [
            libc::SYS_ptrace,
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_enter,
            libc::SYS_io_uring_register,
        ] {
            rules.insert(syscall, vec![]);
        }

        // When network is off, also deny `socket(AF_INET, …)`/`socket(AF_INET6, …)`;
        // AF_UNIX / AF_NETLINK fall through to the Allow default.
        if deny_inet {
            rules.insert(
                libc::SYS_socket,
                vec![
                    SeccompRule::new(vec![domain_is(libc::AF_INET as u64)?])
                        .map_err(map_seccomp)?,
                    SeccompRule::new(vec![domain_is(libc::AF_INET6 as u64)?])
                        .map_err(map_seccomp)?,
                ],
            );
        }

        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        )
        .map_err(map_seccomp)?;
        let program = BpfProgram::try_from(filter).map_err(map_seccomp)?;
        Ok(Some(program))
    }

    /// A condition matching `socket()`'s first argument (the address family).
    fn domain_is(value: u64) -> AgenkitResult<SeccompCondition> {
        SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, value)
            .map_err(map_seccomp)
    }

    fn target_arch() -> Option<TargetArch> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(TargetArch::x86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(TargetArch::aarch64)
        }
        #[cfg(target_arch = "riscv64")]
        {
            Some(TargetArch::riscv64)
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )))]
        {
            None
        }
    }

    fn map_landlock(err: impl std::fmt::Display) -> AgenkitError {
        AgenkitError::config(format!("landlock ruleset: {err}"))
    }

    fn map_seccomp(err: impl std::fmt::Display) -> AgenkitError {
        AgenkitError::config(format!("seccomp filter: {err}"))
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    use pocopine_agenkit_core::AgenkitResult;

    use super::SandboxPolicy;

    /// No-op installer on non-Linux unix targets.
    pub struct SandboxInstaller;

    impl SandboxInstaller {
        pub fn build(_policy: &SandboxPolicy) -> AgenkitResult<Self> {
            Ok(Self)
        }

        pub fn seccomp_only(_policy: &SandboxPolicy) -> AgenkitResult<Self> {
            Ok(Self)
        }

        pub fn apply(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
