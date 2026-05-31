//! `<file-browser-top-bar>` — brand, command search, and the app-bar
//! theme toggle.

use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::storage_command::FileBrowserStorageCommand;

/// `localStorage` key holding the persisted `"dark"` / `"light"`
/// preference. Read at startup by [`apply_saved_theme`].
const THEME_KEY: &str = "cfe-theme";

fn theme_storage() -> pocopine::storage::LocalStorage<String> {
    pocopine::storage::LocalStorage::new(THEME_KEY)
}

/// Whether the persisted preference is dark mode (defaults to light).
fn saved_dark() -> bool {
    theme_storage().get().ok().flatten().as_deref() == Some("dark")
}

/// Reflect `dark` onto `<html data-theme>` — the selector the dark
/// tokens in `app.css` key off of.
fn set_root_theme(dark: bool) {
    let Some(root) = pocopine::dom::document_element() else {
        return;
    };
    if dark {
        let _ = root.set_attribute("data-theme", "dark");
    } else {
        let _ = root.remove_attribute("data-theme");
    }
}

/// Apply the persisted theme to `<html>` at app startup, before the
/// first paint — call from `main()` so a saved dark preference doesn't
/// flash light on reload.
pub fn apply_saved_theme() {
    set_root_theme(saved_dark());
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserTopBar.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon, FileBrowserStorageCommand]
)]
pub struct FileBrowserTopBar {
    /// Mirrors the active theme so the menu can swap its sun/moon icon
    /// and label. Seeded from `localStorage` on setup.
    pub dark: bool,
}

#[handlers]
impl FileBrowserTopBar {
    fn on_setup(&mut self) {
        self.dark = saved_dark();
    }

    /// Flip the theme, reflect it onto `<html>`, and persist it.
    pub fn toggle_theme(&mut self) {
        self.dark = !self.dark;
        set_root_theme(self.dark);
        let _ = theme_storage().set(&if self.dark { "dark" } else { "light" }.to_string());
    }
}
