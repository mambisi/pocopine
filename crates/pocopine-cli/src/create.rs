//! `pocopine create <name>` — scaffold a new project from the embedded starter
//! template (the welcome-page app). Self-contained: the template files are
//! compiled into the binary, so it works offline with no clone/auth.

use std::fs;

use anyhow::{bail, Context, Result};

use crate::args::NewArgs;

/// `(output path relative to the project root, file contents)`. The template's
/// `Cargo.toml` is stored as `Cargo.toml.in` (and dotfiles without the leading
/// dot) so cargo doesn't treat the template dir as a package.
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../templates/app/Cargo.toml.in")),
    ("rust-toolchain.toml", include_str!("../templates/app/rust-toolchain.toml")),
    ("app.css", include_str!("../templates/app/app.css")),
    ("index.html", include_str!("../templates/app/index.html")),
    (".gitignore", include_str!("../templates/app/gitignore")),
    ("AGENTS.md", include_str!("../templates/app/AGENTS.md")),
    ("README.md", include_str!("../templates/app/README.md")),
    (".vscode/extensions.json", include_str!("../templates/app/vscode/extensions.json")),
    ("src/lib.rs", include_str!("../templates/app/src/lib.rs")),
    ("src/WelcomeApp.poco", include_str!("../templates/app/src/WelcomeApp.poco")),
    ("src/WelcomeItem.poco", include_str!("../templates/app/src/WelcomeItem.poco")),
    ("src/Counter.poco", include_str!("../templates/app/src/Counter.poco")),
];

pub fn run(args: &NewArgs) -> Result<()> {
    let crate_name = sanitize(&args.name)?;
    let module = crate_name.replace('-', "_"); // wasm-pack output / js module name
    let target = args.path.join(&args.name);

    if target.exists() {
        bail!("{} already exists", target.display());
    }

    for (rel, contents) in FILES {
        let out = target.join(rel);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        // The template is the `pocopine-app` starter; rename it to this project.
        let rendered = contents
            .replace("pocopine-app", &crate_name)
            .replace("pocopine_app", &module);
        fs::write(&out, rendered).with_context(|| format!("write {}", out.display()))?;
    }

    // Best-effort `git init` (ignore failure — git may be absent).
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&target)
        .status();

    println!("✓ created {}", target.display());
    println!();
    println!("  cd {}", args.name);
    println!("  pocopine dev          # build + serve with live reload");
    Ok(())
}

/// Derive a valid Cargo package name from the project name.
fn sanitize(name: &str) -> Result<String> {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-').to_string();
    if trimmed.is_empty() || !trimmed.starts_with(|c: char| c.is_ascii_alphabetic()) {
        bail!(
            "project name '{name}' is not a valid crate name (start with a letter; use letters, digits, '-' or '_')"
        );
    }
    Ok(trimmed)
}
