//! `<pine-separator>` — visual or semantic divider.
//!
//! Mirrors Radix `<Separator>`. Renders as a `<div>` with
//! `role="separator"` by default; pass `decorative="true"` to
//! drop the role (and `aria-orientation`) for purely visual
//! rules that add no semantic value. Supports horizontal and
//! vertical orientation via `orientation`.
//!
//! ```html
//! <pine-separator></pine-separator>
//! <pine-separator orientation="vertical"></pine-separator>
//! <pine-separator decorative="true"></pine-separator>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[component(template = "PineSeparator.poco", role = "panel")]
pub struct PineSeparator {
    /// `"horizontal"` (default) or `"vertical"`.
    #[prop] pub orientation: String,
    /// `true` → `role="none"` + no `aria-orientation`; omit when
    /// the separator adds real structure screen readers should
    /// announce.
    #[prop] pub decorative: bool,
}

impl Default for PineSeparator {
    fn default() -> Self {
        Self {
            orientation: "horizontal".into(),
            decorative: false,
        }
    }
}

#[handlers]
impl PineSeparator {}
