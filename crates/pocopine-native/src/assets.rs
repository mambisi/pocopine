//! Static-asset root resolution (RFC-104 §6).
//!
//! The native window serves `index.html` + the wasm `pkg/` bundle from
//! a directory on disk. Two modes:
//!
//! * **dev** — `pocopine native dev` exports [`DEV_DIR_ENV`] pointing at
//!   the live project directory, so a rebuild is picked up on reload
//!   without copying anything.
//! * **bundled** — no env var; the shell resolves the Tauri resource
//!   directory instead (that lookup needs an `AppHandle`, so it lives in
//!   `shell.rs` behind the `tauri` feature).
//!
//! Only the dev-side resolution lives here so it stays free of the
//! `tauri` dependency and unit-testable.

use std::path::PathBuf;

/// Environment variable `pocopine native dev` sets to the project
/// directory. When present, it is the static root; when absent, the
/// app is running bundled and resolves its resource directory instead.
pub const DEV_DIR_ENV: &str = "POCOPINE_NATIVE_DEV_DIR";

/// The on-disk dev static root from [`DEV_DIR_ENV`], if set and
/// non-empty. `None` means "not running under `pocopine native dev`" —
/// the caller should fall back to the bundled resource directory.
pub fn dev_dir() -> Option<PathBuf> {
    std::env::var_os(DEV_DIR_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_dir_is_none_when_unset_or_empty() {
        let key = DEV_DIR_ENV;
        let previous = std::env::var_os(key);

        // SAFETY (edition 2024 makes env mutation `unsafe`): single-threaded
        // unit test; no other thread reads the env concurrently, and the
        // prior value is restored before exit.
        unsafe {
            std::env::remove_var(key);
            assert_eq!(dev_dir(), None);

            std::env::set_var(key, "");
            assert_eq!(dev_dir(), None, "empty value is treated as unset");

            std::env::set_var(key, "/tmp/project");
            assert_eq!(dev_dir(), Some(PathBuf::from("/tmp/project")));

            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
