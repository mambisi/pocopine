//! Build-script helper for Pocopine managed client modules.
//!
//! App crates that want rust-analyzer/Cargo to see generated client
//! module bindings can call this from `build.rs`:
//!
//! ```ignore
//! fn main() {
//!     pocopine_client_build::generate().unwrap();
//! }
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn generate() -> Result<PathBuf> {
    let project = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").context(
        "CARGO_MANIFEST_DIR is not set; pocopine_client_build::generate must run from build.rs",
    )?);
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").context(
            "OUT_DIR is not set; pocopine_client_build::generate must run from build.rs",
        )?);

    emit_rerun_if_changed(&project);
    pocopine_client_codegen::write_rust_bindings(&project, out_dir)
}

fn emit_rerun_if_changed(project: &std::path::Path) {
    println!("cargo:rerun-if-changed={}", project.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        project.join("package.json").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        project.join("tsconfig.json").display()
    );
}
