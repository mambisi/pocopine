use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::PocopineConfig;
use crate::tools;

pub fn wasm(path: &Path, release: bool) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("could not resolve project path: {}", path.display()))?;
    println!("▶ wasm-pack build ({})", path.display());
    let project_tools = tools::ProjectTools::load(&path)?;
    let mut cmd = project_tools.wasm_pack().command();
    cmd.arg("build").arg("--target").arg("web");
    if release {
        cmd.arg("--release");
    } else {
        cmd.arg("--dev");
    }
    cmd.current_dir(&path);
    let status = cmd
        .status()
        .context("failed to invoke wasm-pack (is it on $PATH?)")?;
    if !status.success() {
        bail!("wasm-pack build failed with status {status}");
    }
    Ok(())
}

pub fn configured_bins(path: &Path, cfg: &PocopineConfig, release: bool) -> Result<()> {
    for bin in configured_bin_names(cfg) {
        build_bin(path, bin, release)?;
    }
    Ok(())
}

fn configured_bin_names(cfg: &PocopineConfig) -> Vec<&str> {
    let mut bins = Vec::new();
    if let Some(bin) = cfg.bin.as_deref() {
        bins.push(bin);
    }
    if let Some(worker) = cfg.worker_bin.as_deref() {
        if !bins.contains(&worker) {
            bins.push(worker);
        }
    }
    bins
}

fn build_bin(path: &Path, bin: &str, release: bool) -> Result<()> {
    let project = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    let project_tools = tools::ProjectTools::load(&project)?;
    let mut cmd = project_tools.cargo().command();
    cmd.arg("build").arg("--bin").arg(bin);
    if release {
        cmd.arg("--release");
    }
    cmd.current_dir(&project);
    println!(
        "▶ building `{bin}` (cargo build --bin {bin} in {})",
        project.display()
    );
    let status = cmd
        .status()
        .with_context(|| format!("failed to build configured bin `{bin}`"))?;
    if !status.success() {
        bail!("configured bin `{bin}` failed to build with {status}");
    }
    Ok(())
}
