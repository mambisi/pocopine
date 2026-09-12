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
//! `#[watch(name, variant)]`. The template binds
//! `pp-html="svg"` — a reactive field read that re-renders
//! whenever the watcher commits a changed SVG.

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
    /// Computed inline-style for the sized box.
    pub size_style: String,
}

#[handlers]
impl PineIcon {
    pub fn on_setup(&mut self) {
        self.refresh_svg();
        self.refresh_size_style();
    }

    // Compare resolved entries before allocating an owned SVG. Different
    // names or variants can resolve to the same registered entry or miss.
    #[watch(name, variant)]
    fn on_icon_change(name: Change<String>, variant: Change<String>) -> Update<Self, (Self::Svg,)> {
        let (svg, hash) = resolve_icon(&name.current, &variant.current);
        if let (Some(name), Some(variant)) = (name.previous, variant.previous) {
            let variant = if variant.is_empty() {
                "outline"
            } else {
                &variant
            };
            let (_, previous_hash) =
                crate::lookup_with_hash(variant, &name).unwrap_or(crate::EMPTY_ICON);
            if hash == previous_hash {
                return Update::new();
            }
        }
        Update::new().svg(svg.to_string())
    }

    // Size only feeds `size_style`; no need to re-run the lookup.
    #[watch(size)]
    fn on_size_change(size: u32) -> Update<Self, (Self::SizeStyle,)> {
        Update::new().size_style(size_style(size))
    }
}

impl PineIcon {
    /// Seed the SVG before the first template render. Subsequent updates
    /// compare registered hashes using the watcher's previous inputs.
    fn refresh_svg(&mut self) {
        self.svg = resolve_icon(&self.name, &self.variant).0.to_string();
    }

    /// Recompute the inline `width/height` style. Unset (`0`)
    /// emits NO inline style — the `.pine-icon` class defaults the
    /// box to `1em`, and keeping the style attribute empty is what
    /// lets app CSS override the size without `!important`. Same
    /// no-op guard pattern — resizing a primitive without actually
    /// changing `size` shouldn't retrigger downstream effects.
    fn refresh_size_style(&mut self) {
        let next = size_style(self.size);
        if self.size_style != next {
            self.size_style = next;
        }
    }
}

fn size_style(size: u32) -> String {
    if size == 0 {
        String::new()
    } else {
        format!("width:{size}px;height:{size}px")
    }
}

fn resolve_icon(name: &str, variant: &str) -> (&'static str, u64) {
    let variant = if variant.is_empty() {
        "outline"
    } else {
        variant
    };
    match crate::lookup_with_hash(variant, name) {
        Some(pair) => pair,
        None => {
            if cfg!(debug_assertions) && !name.is_empty() {
                ::web_sys::console::warn_1(
                    &format!(
                        "pine-icons: `{variant}/{name}` not registered — add it to `register_icons![…]`"
                    )
                    .into(),
                );
            }
            crate::EMPTY_ICON
        }
    }
}
