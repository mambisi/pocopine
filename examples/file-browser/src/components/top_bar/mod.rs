//! `<file-browser-top-bar>` — brand, search field, account chip.
//! Pure presentation; the search input is intentionally non-functional
//! in the demo (no full-text index yet).

use pine::{PineAvatarFallback, PineAvatarRoot, PineInput};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserTopBar.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon, PineInput, PineAvatarRoot, PineAvatarFallback]
)]
pub struct FileBrowserTopBar {
    /// Local search field state — not wired to a backend yet, but the
    /// `pine-input` two-way binding keeps the value reactive so we
    /// can highlight the toolbar when the field is dirty.
    #[model]
    pub query: String,
}

#[handlers]
impl FileBrowserTopBar {}
