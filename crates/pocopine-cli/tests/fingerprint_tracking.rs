//! RFC-100 §6 — proof that the `asset!` rebuild-tracking mechanism
//! works: a proc macro that emits
//! `const _: Option<&str> = ::core::option_env!("POCOPINE_ASSETS_FINGERPRINT");`
//! into its expansion makes cargo **re-expand the calling crate**
//! when that env var changes between builds, while an otherwise
//! identical macro without the emission does not (the negative
//! control proving the `option_env!` reference is load-bearing).
//!
//! Mechanism under test: rustc records every `env!`/`option_env!`
//! expansion as an env dependency in the crate's dep-info
//! (`# env-dep:` lines in the `.d` file), and cargo includes those
//! env values in the unit fingerprint — so a changed value dirties
//! the crate, recompiles it, and re-runs every proc-macro expansion
//! inside it. This is exactly what `pocopine-macros::asset!` emits
//! when `POCOPINE_ASSETS_FINGERPRINT` is set (see
//! `crates/pocopine-macros/src/assets.rs`); the test reproduces the
//! construct with a dependency-free macro so the whole proof builds
//! in seconds.
//!
//! Layout built in a tempdir:
//!
//! ```text
//! workspace/
//!   fp-macro/        proc-macro: tracked!() emits the const + the env
//!                    value seen at expansion; untracked!() emits only
//!                    the env value (no option_env! reference)
//!   app-tracked/     bin printing tracked!()
//!   app-untracked/   bin printing untracked!()   (separate crate so a
//!                    tracked-triggered rebuild can't re-expand it)
//! ```
//!
//! Requires `cargo` on PATH (always true under `cargo test`).

use std::path::Path;
use std::process::Command;

const ENV: &str = "POCOPINE_ASSETS_FINGERPRINT";

#[test]
fn option_env_in_proc_macro_expansion_retriggers_expansion_on_env_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_workspace(root);

    // Build + run both bins with fingerprint AAA.
    let tracked_a = run_bin(root, "app-tracked", Some("AAA"));
    let untracked_a = run_bin(root, "app-untracked", Some("AAA"));
    assert_eq!(tracked_a, "expanded-with:AAA");
    assert_eq!(untracked_a, "expanded-with:AAA");

    // Change ONLY the env var. No source file is touched.
    let tracked_b = run_bin(root, "app-tracked", Some("BBB"));
    let untracked_b = run_bin(root, "app-untracked", Some("BBB"));

    // The tracked crate re-expanded: the macro saw the new value.
    assert_eq!(
        tracked_b, "expanded-with:BBB",
        "option_env! emission failed to retrigger expansion on env change — \
         the asset! rebuild-tracking mechanism is broken"
    );
    // Negative control: without the option_env! reference cargo has no
    // env-dep recorded, reuses the cached build, and the stale
    // expansion survives. This is what proves the emitted const is
    // load-bearing (and why bare-cargo builds need the dev-server 409
    // backstop).
    assert_eq!(
        untracked_b, "expanded-with:AAA",
        "untracked control rebuilt anyway — the test no longer isolates the mechanism"
    );
}

/// `cargo run -q -p <bin>` in the temp workspace, with the
/// fingerprint env set/unset, returning trimmed stdout.
fn run_bin(root: &Path, bin: &str, fingerprint: Option<&str>) -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.arg("run").arg("-q").arg("-p").arg(bin);
    cmd.current_dir(root);
    // Hermetic-ish: keep the workspace's own target dir.
    cmd.env_remove("CARGO_TARGET_DIR");
    match fingerprint {
        Some(value) => {
            cmd.env(ENV, value);
        }
        None => {
            cmd.env_remove(ENV);
        }
    }
    let output = cmd.output().expect("spawn cargo run");
    assert!(
        output.status.success(),
        "cargo run -p {bin} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write_workspace(root: &Path) {
    write(
        root,
        "Cargo.toml",
        r#"[workspace]
resolver = "2"
members = ["fp-macro", "app-tracked", "app-untracked"]
"#,
    );

    // ── proc-macro crate (no dependencies) ──
    write(
        root,
        "fp-macro/Cargo.toml",
        r#"[package]
name = "fp-macro"
version = "0.0.0"
edition = "2021"

[lib]
proc-macro = true
"#,
    );
    write(
        root,
        "fp-macro/src/lib.rs",
        r#"use proc_macro::TokenStream;

fn seen() -> String {
    std::env::var("POCOPINE_ASSETS_FINGERPRINT").unwrap_or_else(|_| "unset".into())
}

/// Mirrors `pocopine_macros::asset!`'s tracking emission: the env
/// value seen at expansion time, plus the option_env! reference that
/// records the env dependency.
#[proc_macro]
pub fn tracked(_: TokenStream) -> TokenStream {
    format!(
        "{{ const _: ::core::option::Option<&str> = \
           ::core::option_env!(\"POCOPINE_ASSETS_FINGERPRINT\"); \
           \"expanded-with:{}\" }}",
        seen()
    )
    .parse()
    .unwrap()
}

/// Negative control: same expansion-time read, no tracking emission.
#[proc_macro]
pub fn untracked(_: TokenStream) -> TokenStream {
    format!("\"expanded-with:{}\"", seen()).parse().unwrap()
}
"#,
    );

    // ── consumer bins, one per macro ──
    for (name, macro_name) in [("app-tracked", "tracked"), ("app-untracked", "untracked")] {
        write(
            root,
            &format!("{name}/Cargo.toml"),
            &format!(
                r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2021"

[dependencies]
fp-macro = {{ path = "../fp-macro" }}
"#
            ),
        );
        write(
            root,
            &format!("{name}/src/main.rs"),
            &format!("fn main() {{\n    println!(\"{{}}\", fp_macro::{macro_name}!());\n}}\n"),
        );
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}
