//! RFC-100 — end-to-end test of `pocopine assets push` against a
//! MinIO testcontainer (same infra pattern as
//! `pocopine-storage-s3/tests/minio.rs`).
//!
//! Drives the real binary (`CARGO_BIN_EXE_pocopine`) with a temp
//! project whose `[package.metadata.pocopine.assets]` block points at
//! the container, credentials supplied via the
//! `POCOPINE_ASSETS_ACCESS_KEY_ID` / `POCOPINE_ASSETS_SECRET_ACCESS_KEY`
//! env override (so no `~/.pocopine/credentials.toml` is touched —
//! `HOME` is additionally pointed at a tempdir as belt and braces).
//!
//! The push-twice + edit sequence pins the hash-diff contract:
//!   1. fresh bucket  → everything uploads
//!   2. same tree     → everything skips (content-addressed keys exist)
//!   3. one file edit → exactly the new hash uploads; old keys remain
//!
//! Requires a working Docker daemon.

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::process::Command;

use testcontainers::runners::AsyncRunner;
use testcontainers_modules::minio::MinIO;

const MINIO_PORT: u16 = 9000;
const MINIO_ROOT_USER: &str = "minioadmin";
const MINIO_ROOT_PASSWORD: &str = "minioadmin";

#[tokio::test]
async fn assets_push_uploads_skips_and_reuploads_on_edit() {
    let container = MinIO::default()
        .start()
        .await
        .expect("start minio testcontainer");
    let port = container
        .get_host_port_ipv4(MINIO_PORT)
        .await
        .expect("minio container port");
    let endpoint = format!("http://127.0.0.1:{port}");

    let project = tempfile::tempdir().expect("temp project");
    let fake_home = tempfile::tempdir().expect("temp home");
    write_project(project.path(), &endpoint);

    // 1. Fresh bucket: both assets upload (push also creates the
    //    bucket via ensure_bucket).
    let out = push(project.path(), fake_home.path());
    assert!(
        out.contains("2 uploaded, 0 skipped"),
        "first push should upload everything, got:\n{out}"
    );
    assert!(out.contains("assets/"), "summary should name keys:\n{out}");

    // 2. Same tree: nothing to do — the hash-diff found every key.
    let out = push(project.path(), fake_home.path());
    assert!(
        out.contains("0 uploaded, 2 skipped"),
        "second push should skip everything, got:\n{out}"
    );

    // 3. Edit one file: its content hash changes, so exactly one new
    //    key uploads; the unchanged file still skips.
    std::fs::write(project.path().join("assets/logo.svg"), b"<svg>v2</svg>").unwrap();
    let out = push(project.path(), fake_home.path());
    assert!(
        out.contains("1 uploaded, 1 skipped"),
        "edited file should re-upload under its new hash, got:\n{out}"
    );
}

/// Run `pocopine assets push --path <project>` with env-supplied
/// credentials, returning combined stdout+stderr. Panics on non-zero
/// exit.
fn push(project: &Path, home: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_pocopine"))
        .args(["assets", "push", "--path"])
        .arg(project)
        .env("HOME", home)
        .env("POCOPINE_ASSETS_ACCESS_KEY_ID", MINIO_ROOT_USER)
        .env("POCOPINE_ASSETS_SECRET_ACCESS_KEY", MINIO_ROOT_PASSWORD)
        .output()
        .expect("spawn pocopine assets push");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "pocopine assets push failed:\n{combined}"
    );
    combined
}

fn write_project(root: &Path, endpoint: &str) {
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "assets-push-it"
version = "0.0.0"
edition = "2021"

[package.metadata.pocopine.assets]
endpoint = "{endpoint}"
bucket = "pocopine-cli-push-it-{}"
"#,
            std::process::id()
        ),
    )
    .unwrap();
    let assets = root.join("assets");
    std::fs::create_dir_all(assets.join("blog")).unwrap();
    std::fs::write(assets.join("logo.svg"), b"<svg>v1</svg>").unwrap();
    std::fs::write(assets.join("blog/clip.webm"), b"webm-bytes").unwrap();
}
