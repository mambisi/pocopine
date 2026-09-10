//! Supply the upstream Tree-sitter mini-sysroot to grammar crates whose build
//! scripts do not yet forward tree-sitter-language's WASM header metadata.
use crate::tools::ProjectTools;
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn configure(project: &Path, tools: &ProjectTools, build: &mut Command) -> Result<()> {
    let output = tools
        .cargo()
        .command()
        .args([
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "wasm32-unknown-unknown",
        ])
        .current_dir(project)
        .output()
        .context("resolve WASM C dependencies")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if let Some(headers) = headers(&metadata, project)? {
        let quoted = headers
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        // cc accepts both forms; use the higher-priority hyphenated key while
        // preserving whichever target-specific flags the caller configured.
        let flags = std::env::var("CFLAGS_wasm32-unknown-unknown")
            .or_else(|_| std::env::var("CFLAGS_wasm32_unknown_unknown"))
            .unwrap_or_default();
        build.env("CC_SHELL_ESCAPED_FLAGS", "1");
        build.env(
            "CFLAGS_wasm32-unknown-unknown",
            format!("{flags} -I\"{quoted}\""),
        );
        println!(
            "✓ Tree-sitter WASM headers: {} (requires Clang and llvm-ar)",
            headers.display()
        );
    }
    Ok(())
}
fn headers(metadata: &serde_json::Value, project: &Path) -> Result<Option<PathBuf>> {
    let Some(packages) = metadata["packages"].as_array() else {
        return Ok(None);
    };
    let Some(root) = packages.iter().find(|p| {
        p["manifest_path"]
            .as_str()
            .is_some_and(|p| Path::new(p).parent() == Some(project))
    }) else {
        return Ok(None);
    };
    let Some(nodes) = metadata["resolve"]["nodes"].as_array() else {
        return Ok(None);
    };
    let mut pending = vec![root["id"].as_str().unwrap()];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        if let Some(package) = packages.iter().find(|p| p["id"].as_str() == Some(id))
            && package["name"] == "tree-sitter-language"
        {
            let manifest = Path::new(package["manifest_path"].as_str().unwrap());
            let headers = manifest.parent().unwrap().join("wasm/include");
            if !headers.join("stdlib.h").is_file() {
                bail!(
                    "Tree-sitter dependency lacks WASM headers: {}",
                    headers.display()
                );
            }
            return Ok(Some(headers));
        }
        if let Some(node) = nodes.iter().find(|n| n["id"].as_str() == Some(id))
            && let Some(deps) = node["dependencies"].as_array()
        {
            pending.extend(deps.iter().filter_map(serde_json::Value::as_str));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn does_not_take_headers_from_an_unrelated_workspace_member() {
        let root = std::env::temp_dir().join("pocopine-app-without-parser");
        let metadata = serde_json::json!({
            "packages": [
                {"id":"app", "name":"app", "manifest_path":root.join("Cargo.toml")},
                {"id":"other", "name":"other", "manifest_path":root.join("other/Cargo.toml")},
                {"id":"language", "name":"tree-sitter-language", "manifest_path":root.join("missing/Cargo.toml")}
            ],
            "resolve":{"nodes":[
                {"id":"app", "dependencies":[]},
                {"id":"other", "dependencies":["language"]},
                {"id":"language", "dependencies":[]}
            ]}
        });
        assert_eq!(headers(&metadata, &root).unwrap(), None);
        let mut reachable = metadata;
        reachable["resolve"]["nodes"][0]["dependencies"] = serde_json::json!(["language"]);
        assert!(
            headers(&reachable, &root)
                .unwrap_err()
                .to_string()
                .contains("lacks WASM headers")
        );
    }
}
