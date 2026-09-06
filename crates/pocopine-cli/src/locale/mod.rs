//! Locale compilation and delivery shared by the application build entrypoints.

use std::{path::PathBuf, process::Command};

use pocopine_locale::LocaleManifest;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
mod server;
#[cfg(not(target_arch = "wasm32"))]
use server as platform;
#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
use client as platform;

pub use platform::{editor_completions, editor_hover, load, prepare, publish, run};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prepared {
    pub rust: PathBuf,
    pub directory: PathBuf,
    pub runtime_data: PathBuf,
    pub features: Vec<String>,
    pub manifest: LocaleManifest,
}

impl Prepared {
    pub fn configure(&self, command: &mut Command) {
        command.env("POCOPINE_LOCALE_RS", &self.rust);
        command.env("POCOPINE_LOCALE_DATA_DIR", &self.runtime_data);
        if !self.features.is_empty() {
            command.arg("--features").arg(self.features.join(","));
        }
    }
}

/// Embed metadata beside the exact hashed bundle, never through a mutable
/// manifest fetch that could cross deployments. Catalog keys stay server-side.
pub fn inject_html(html: &str, prepared: &Prepared) -> anyhow::Result<String> {
    let payload = platform::shell_payload(prepared)?;
    let json = serde_json::to_string(&payload)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    let script = include_str!("../../assets/locale-loader.js");
    let addition = format!(
        "<script type=\"application/json\" id=\"pp-locale-manifest\">{json}</script>\n<script>\n{script}</script>\n"
    );
    let at = html.find("</head>").ok_or_else(|| {
        anyhow::anyhow!("locale application index.html must contain a closing </head>")
    })?;
    let mut html = html.to_owned();
    html.insert_str(at, &addition);
    Ok(html)
}
