//! Best-effort cgroup v2 enforcement (`memory.max` + `pids.max`).
//!
//! rlimits (Phase 4) are the always-on floor; a cgroup adds limits that survive
//! `RLIMIT_AS`-incompatible workloads. It requires a *delegated, writable* cgroup
//! subtree, which many unprivileged hosts don't grant — so this degrades
//! cleanly: if a child cgroup can't be created or no limit can be set, it
//! returns `None` and the rlimits alone apply.
//!
//! Linux-only; a no-op stub on other unix targets.

#[cfg(target_os = "linux")]
pub use linux::CgroupGuard;
#[cfg(not(target_os = "linux"))]
pub use other::CgroupGuard;

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A transient child cgroup; removed on drop.
    pub struct CgroupGuard {
        path: PathBuf,
    }

    impl CgroupGuard {
        /// Create a child cgroup and apply whichever limits the host allows.
        /// Returns `None` (→ rely on rlimits) when nothing could be enforced.
        pub fn create(memory_bytes: Option<u64>, max_pids: Option<u64>) -> Option<Self> {
            if memory_bytes.is_none() && max_pids.is_none() {
                return None;
            }
            let base = current_cgroup_dir()?;
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("agenkitty.{}.{id}", std::process::id()));
            std::fs::create_dir(&path).ok()?;

            let memory_set = memory_bytes
                .map(|bytes| std::fs::write(path.join("memory.max"), bytes.to_string()).is_ok())
                .unwrap_or(false);
            let pids_set = max_pids
                .map(|count| std::fs::write(path.join("pids.max"), count.to_string()).is_ok())
                .unwrap_or(false);

            if !memory_set && !pids_set {
                // No controller is delegated here — drop the empty cgroup and
                // let the rlimits do the work instead.
                let _ = std::fs::remove_dir(&path);
                return None;
            }
            Some(Self { path })
        }

        /// Move a (running) child process into this cgroup. Best-effort.
        pub fn add_pid(&self, pid: u32) {
            let _ = std::fs::write(self.path.join("cgroup.procs"), pid.to_string());
        }
    }

    impl Drop for CgroupGuard {
        fn drop(&mut self) {
            // The cgroup must be empty (the child has exited and been reaped)
            // before rmdir succeeds; a leftover dir is harmless.
            let _ = std::fs::remove_dir(&self.path);
        }
    }

    /// Resolve this process's cgroup v2 directory from `/proc/self/cgroup`.
    fn current_cgroup_dir() -> Option<PathBuf> {
        let contents = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let relative = contents
            .lines()
            .find_map(|line| line.strip_prefix("0::"))?
            .trim();
        let relative = relative.strip_prefix('/').unwrap_or(relative);
        Some(PathBuf::from("/sys/fs/cgroup").join(relative))
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    /// No-op cgroup guard on non-Linux unix targets.
    pub struct CgroupGuard;

    impl CgroupGuard {
        pub fn create(_memory_bytes: Option<u64>, _max_pids: Option<u64>) -> Option<Self> {
            None
        }

        pub fn add_pid(&self, _pid: u32) {}
    }
}
