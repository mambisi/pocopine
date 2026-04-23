//! Reusable preview / code tab wrapper for primitive demos.
//!
//! Usage in `Showcase.poco`:
//!
//! ```html
//! <showcase-card title="Button" slug="button">
//!   <template pp-slot="preview">
//!     <pine-button>Click me</pine-button>
//!   </template>
//!   <template pp-slot="code">
//!     <pre><code>&lt;pine-button&gt;Click me&lt;/pine-button&gt;</code></pre>
//!   </template>
//! </showcase-card>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ShowcaseCard.poco",
    style = "showcase_card.css",
    role = "panel"
)]
pub struct ShowcaseCard {
    /// Heading shown in the card header.
    #[prop]
    pub title: String,
    /// Optional URL fragment id — authors can deep-link to a card.
    #[prop]
    pub slug: String,
    /// Currently visible tab: `"preview"` (default) or `"code"`.
    pub tab: String,
}

#[handlers]
impl ShowcaseCard {
    pub fn on_setup(&mut self) {
        if self.tab.is_empty() {
            self.tab = "preview".into();
        }
    }

    pub fn show_preview(&mut self) {
        self.tab = "preview".into();
    }

    pub fn show_code(&mut self) {
        self.tab = "code".into();
    }
}
