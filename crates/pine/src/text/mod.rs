//! `<pine-text>` — typography primitive with measurement-backed
//! truncation state powered by [`pocopine::text`].
//!
//! Visual truncation is delegated to CSS `-webkit-line-clamp`, which
//! the browser applies pixel-perfectly (Chrome / Safari / Firefox
//! since 2020). The engine's job is to tell your CSS and Rust
//! handlers **whether clamping actually happened** — CSS alone has
//! no way to signal that.
//!
//! Props:
//!
//! - `lines` — clamp to N lines. `0` (default) disables.
//! - `max-width` — explicit measurement width in CSS pixels. `0.0`
//!   resolves the nearest ancestor's content-box width at layout
//!   time, so the state reflows with its container.
//! - `line-height` — px line height used for height math. `0.0`
//!   defaults to `20.0` (authors still control visual line-height
//!   via CSS).
//!
//! Auto-tracked state (exposed as `data-*`):
//!
//! - `data-truncated` — `true` when the text overflowed the clamp.
//! - `data-line-count` — measured wrapped-line count at the current
//!   width.
//!
//! ```html
//! <pine-text lines="2">
//!   A long paragraph that needs to be shortened...
//! </pine-text>
//! ```

use pocopine::prelude::*;
use pocopine::text::{layout, prepare, CanvasMeasurer, Font, PrepareOptions};
use pocopine::{current_scope_id, refs, tick};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

#[derive(Serialize, Deserialize)]
#[component(template = "PineText.poco", role = "text", display = "contents")]
pub struct PineText {
    /// Truncate to N wrapped lines. `0` disables truncation.
    #[prop]
    pub lines: u32,
    /// Explicit max-width in CSS pixels. `0.0` reads
    /// `root.clientWidth` at layout time.
    #[prop]
    pub max_width: f64,
    /// Line-height in px (for height math only). `0.0` defaults to 20.
    #[prop]
    pub line_height: f64,

    /// Set to `true` when the text overflowed the `lines` clamp.
    pub truncated: bool,
    /// Measured wrapped-line count at the last layout pass.
    pub line_count: u32,
}

impl Default for PineText {
    fn default() -> Self {
        Self {
            lines: 0,
            max_width: 0.0,
            line_height: 0.0,
            truncated: false,
            line_count: 0,
        }
    }
}

#[handlers]
impl PineText {
    fn on_setup(&mut self) {
        if self.line_height <= 0.0 {
            self.line_height = 20.0;
        }
    }

    fn on_ready(&self) {
        // Apply the clamp style synchronously — the scope is live
        // in on_ready. Measurement defers because `handle.update`
        // inside on_ready is the RefCell double-borrow trap.
        let Some(html_el) = self.resolve_root() else {
            return;
        };
        apply_clamp_style(&html_el, self.lines);

        let handle = this::<Self>();
        tick::next(move || {
            handle.update(|s| s.measure_now(&html_el));
        });
    }

    #[watch(lines)]
    fn on_lines_change(&mut self, new: u32, _prev: Option<u32>) {
        let Some(html_el) = self.resolve_root() else {
            return;
        };
        apply_clamp_style(&html_el, new);
        self.schedule_measure(html_el);
    }

    #[watch(max_width)]
    fn on_max_width_change(&mut self, _new: f64, _prev: Option<f64>) {
        let Some(html_el) = self.resolve_root() else {
            return;
        };
        self.schedule_measure(html_el);
    }
}

impl PineText {
    fn resolve_root(&self) -> Option<HtmlElement> {
        let scope = current_scope_id()?;
        let el = refs::get_on(scope, "root")?;
        el.dyn_into::<HtmlElement>().ok()
    }

    fn schedule_measure(&self, html_el: HtmlElement) {
        let handle = this::<Self>();
        tick::next(move || {
            handle.update(|s| s.measure_now(&html_el));
        });
    }

    fn measure_now(&mut self, html_el: &HtmlElement) {
        let text = html_el.text_content().unwrap_or_default();
        let source = text.trim();
        if source.is_empty() {
            self.line_count = 0;
            self.truncated = false;
            return;
        }

        let font = Font(read_computed_font(html_el));
        let max_w = if self.max_width > 0.0 {
            self.max_width
        } else {
            available_width(html_el)
        };
        if max_w <= 0.0 {
            return;
        }

        let measurer = CanvasMeasurer;
        let prepared = prepare(source, &font, PrepareOptions::default(), &measurer);
        let result = layout(&prepared, max_w, self.line_height);

        self.line_count = result.line_count;
        self.truncated = self.lines > 0 && result.line_count > self.lines;
    }
}

/// Apply or clear the CSS line-clamp inline styles. The browser
/// visually truncates at exactly the right pixel; our engine runs
/// in parallel to decide *whether* it truncated.
fn apply_clamp_style(el: &HtmlElement, lines: u32) {
    let style = el.style();
    if lines > 0 {
        let _ = style.set_property("display", "-webkit-box");
        let _ = style.set_property("-webkit-box-orient", "vertical");
        let _ = style.set_property("-webkit-line-clamp", &lines.to_string());
        let _ = style.set_property("line-clamp", &lines.to_string());
        let _ = style.set_property("overflow", "hidden");
    } else {
        let _ = style.remove_property("display");
        let _ = style.remove_property("-webkit-box-orient");
        let _ = style.remove_property("-webkit-line-clamp");
        let _ = style.remove_property("line-clamp");
        let _ = style.remove_property("overflow");
    }
}

/// Find the nearest ancestor with a measurable content-box width.
///
/// The template's root `<span>` is inline (and the `<pine-text>`
/// host is `display: contents`), so `clientWidth` on either one
/// is `0`. Climb until we hit a block-like ancestor and read its
/// resolved content-box width — `clientWidth` would include the
/// ancestor's padding, which the browser strips before wrapping.
fn available_width(el: &HtmlElement) -> f64 {
    let mut cursor: Option<Element> = el.parent_element();
    while let Some(p) = cursor {
        let w = content_width_px(&p);
        if w > 0.0 {
            return w;
        }
        cursor = p.parent_element();
    }
    0.0
}

fn content_width_px(el: &Element) -> f64 {
    let Some(win) = web_sys::window() else {
        return 0.0;
    };
    let Ok(Some(cs)) = win.get_computed_style(el) else {
        return 0.0;
    };
    let raw = cs.get_property_value("width").ok().unwrap_or_default();
    raw.trim()
        .trim_end_matches("px")
        .parse::<f64>()
        .unwrap_or(0.0)
}

/// Read the root element's computed font as a CSS shorthand.
///
/// `getComputedStyle(el).font` returns `""` in most browsers when
/// individual properties diverge, so build the shorthand from
/// parts instead.
fn read_computed_font(el: &HtmlElement) -> String {
    let fallback = "16px system-ui".to_string();
    let Some(win) = web_sys::window() else {
        return fallback;
    };
    let element: &Element = el.as_ref();
    let Ok(Some(cs)) = win.get_computed_style(element) else {
        return fallback;
    };
    let prop = |name: &str| cs.get_property_value(name).ok().unwrap_or_default();

    let style = prop("font-style");
    let weight = prop("font-weight");
    let size = prop("font-size");
    let family = prop("font-family");

    let mut out = String::new();
    if !style.is_empty() && style != "normal" {
        out.push_str(&style);
        out.push(' ');
    }
    if !weight.is_empty() && weight != "400" && weight != "normal" {
        out.push_str(&weight);
        out.push(' ');
    }
    if size.is_empty() {
        out.push_str("16px ");
    } else {
        out.push_str(&size);
        out.push(' ');
    }
    if family.is_empty() {
        out.push_str("system-ui");
    } else {
        out.push_str(&family);
    }
    out
}
