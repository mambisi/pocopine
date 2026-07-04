//! `<pine-icon>` — render a pre-registered Tabler icon.
//!
//! The primitive itself carries no icon data; it reads from the
//! process-wide registry populated by [`crate::register_icons!`].
//! That separation is what lets the WASM binary stay small: the
//! 5000-icon set is vendored, but only the names actually listed
//! in `register_icons!` get linked in.
//!
//! ## Why the reactive pattern uses `#[watch]` + a field
//!
//! pocopine's template expression evaluator invokes `#[handlers]`
//! methods through `invoke_handler`, which discards the return
//! value (always `JsValue::UNDEFINED`). So `pp-html="svg()"` can't
//! read a method result directly. Instead the component holds a
//! computed `svg` *field*, recomputed from the props via
//! `#[watch(name)]` / `#[watch(variant)]`. The template binds
//! `pp-html="svg"` — a reactive field read that re-renders
//! whenever the watchers overwrite it.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineIcon.poco",
    style = "PineIcon.css",
    role = "visual",
    display = "contents"
)]
pub struct PineIcon {
    /// Icon name in kebab-case (e.g. `"chevron-down"`). Must be
    /// registered via [`crate::register_icons!`].
    #[prop]
    pub name: String,
    /// `"outline"` (default) or `"filled"`. Empty string falls
    /// back to `"outline"` so authors can omit the attribute for
    /// the common case.
    #[prop]
    pub variant: String,
    /// Render size in CSS pixels. Unset (`0`) writes no inline
    /// style at all: the icon's box defaults to `1em` via CSS, so
    /// it rides the surrounding font-size and app stylesheets can
    /// resize it without `!important`. An explicit size pins the
    /// box in px via inline style.
    #[prop]
    pub size: u32,
    /// Computed SVG body, fed to `pp-html` in the template.
    /// Kept as a field (not a method) because the handler
    /// dispatch discards return values.
    pub svg: String,
    /// Precomputed FNV-1a hash of the currently rendered SVG.
    /// Skips the full string compare when deciding whether a
    /// prop change actually resolved to a new SVG — O(1)
    /// instead of O(n), meaningful when an app registers
    /// illustration-sized icons and flips between them.
    #[serde(skip)]
    pub svg_hash: u64,
    /// Computed inline-style for the sized box.
    pub size_style: String,
}

#[handlers]
impl PineIcon {
    pub fn on_setup(&mut self) {
        self.refresh_svg();
        self.refresh_size_style();
    }

    // Name and variant both influence which SVG to look up, but
    // the resulting string often doesn't actually change — e.g.,
    // typing a name that's still an unregistered miss leaves
    // `svg` at `""`. The guard inside `refresh_svg` skips the
    // proxy write in that case, which avoids redundantly firing
    // every effect that reads `svg` (`pp-html`, devtools, etc.).
    #[watch(name)]
    fn on_name_change(&mut self, _new: String, _old: Option<String>) {
        self.refresh_svg();
    }

    #[watch(variant)]
    fn on_variant_change(&mut self, _new: String, _old: Option<String>) {
        self.refresh_svg();
    }

    // Size only feeds `size_style`; no need to re-run the lookup.
    #[watch(size)]
    fn on_size_change(&mut self, _new: u32, _old: Option<u32>) {
        self.refresh_size_style();
    }
}

impl PineIcon {
    /// Resolve `name` + `variant` to an SVG string and store it.
    /// Uses the compile-time FNV-1a hash carried on each
    /// registered entry for O(1) change detection — avoids a
    /// byte-by-byte string compare and, critically, avoids the
    /// `to_string()` allocation when the resolved icon didn't
    /// actually change (e.g. typing into a filter that still
    /// resolves to the same name, or toggling between props
    /// that happen to pick the same icon).
    fn refresh_svg(&mut self) {
        let v = if self.variant.is_empty() {
            "outline"
        } else {
            self.variant.as_str()
        };
        let (next_svg, next_hash) = match crate::lookup_with_hash(v, &self.name) {
            Some(pair) => pair,
            None => {
                if cfg!(debug_assertions) && !self.name.is_empty() {
                    ::web_sys::console::warn_1(
                        &format!(
                            "pine-icons: `{v}/{}` not registered — add it to `register_icons![…]`",
                            self.name
                        )
                        .into(),
                    );
                }
                crate::EMPTY_ICON
            }
        };
        if next_hash != self.svg_hash {
            // Only allocate on an actual value change. &'static str
            // → owned String because the proxy-reactive `svg` field
            // is owned — we can't point proxies at static borrows.
            self.svg = next_svg.to_string();
            self.svg_hash = next_hash;
        }
    }

    /// Recompute the inline `width/height` style. Unset (`0`)
    /// emits NO inline style — the `.pine-icon` class defaults the
    /// box to `1em`, and keeping the style attribute empty is what
    /// lets app CSS override the size without `!important`. Same
    /// no-op guard pattern — resizing a primitive without actually
    /// changing `size` shouldn't retrigger downstream effects.
    fn refresh_size_style(&mut self) {
        let next = if self.size == 0 {
            String::new()
        } else {
            let s = self.size;
            format!("width:{s}px;height:{s}px")
        };
        if self.size_style != next {
            self.size_style = next;
        }
    }
}
