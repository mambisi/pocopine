//! A single copyable install command, shown right under the hero —
//! the pocopine analogue of Laravel's `laravel new my-app`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "InstallCmd.poco", style = "install_cmd.css")]
pub struct InstallCmd {
    pub cmd: String,
    pub copied: bool,
}

impl Default for InstallCmd {
    fn default() -> Self {
        Self {
            cmd: "cargo install pocopine-cli".into(),
            copied: false,
        }
    }
}

#[handlers]
impl InstallCmd {
    pub fn copy(&mut self) {
        if let Some(win) = web_sys::window() {
            let _ = win.navigator().clipboard().write_text(&self.cmd);
            self.copied = true;
        }
    }
}
