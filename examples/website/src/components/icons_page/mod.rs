//! `/components/icons` — a lucide.dev-style Tabler icon gallery, laid out
//! in three columns: a left customizer (size + variant), the centre search
//! grid, and a docked detail panel on the right. The ~6,092 names are
//! baked into the wasm for instant client-side search; the SVGs are
//! streamed from the server and mask-rendered so they follow the theme.
//! Selecting an icon fills the detail panel with copy-ready name /
//! `<pine-icon>` tag / `icon!` macro.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::gen_code::icons;

/// Cap on rendered tiles per query — search filters the whole catalogue,
/// but only the first N matches are painted.
const PAGE: usize = 120;

#[derive(Serialize, Deserialize)]
#[component(template = "IconsPage.poco", style = "icons_page.css", role = "panel")]
pub struct IconsPage {
    pub query: String,
    /// `"outline"` | `"filled"`.
    pub variant: String,
    /// Tile glyph render size in px (customizer slider).
    pub size: u32,
    /// `--icon-size:Npx` for the grid container (cascades to every tile).
    pub grid_style: String,
    /// `"24px"` readout next to the slider.
    pub size_label: String,
    pub results: Vec<String>,
    pub count_label: String,
    /// Selected icon name (mask url + name + copy snippets).
    pub selected: String,
    /// Whether the detail panel is open (an icon is selected).
    pub has_selection: bool,
    /// Copy-ready snippets for the selected icon.
    pub selected_tag: String,
    pub selected_macro: String,
    /// Last-copied confirmation.
    pub copied: String,
}

impl Default for IconsPage {
    fn default() -> Self {
        Self {
            query: String::new(),
            variant: "outline".into(),
            size: 24,
            grid_style: "--icon-size:24px".into(),
            size_label: "24px".into(),
            results: Vec::new(),
            count_label: String::new(),
            selected: String::new(),
            has_selection: false,
            selected_tag: String::new(),
            selected_macro: String::new(),
            copied: String::new(),
        }
    }
}

#[handlers]
impl IconsPage {
    pub fn on_setup(&mut self) {
        self.recompute();
    }

    pub fn search(&mut self, ev: web_sys::Event) {
        if let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        {
            self.query = input.value();
            self.recompute();
        }
    }

    pub fn set_variant(&mut self, variant: String) {
        self.variant = variant;
        self.rebuild_selected();
        self.recompute();
    }

    /// Customizer size slider — re-cascade the glyph size via a CSS var.
    pub fn set_size(&mut self, ev: web_sys::Event) {
        if let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        {
            self.size = input.value().parse().unwrap_or(24).clamp(12, 64);
            self.grid_style = format!("--icon-size:{}px", self.size);
            self.size_label = format!("{}px", self.size);
            self.rebuild_selected();
        }
    }

    /// Delegated grid click — open the detail panel for the clicked tile.
    pub fn on_pick(&mut self, ev: web_sys::MouseEvent) {
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let Ok(Some(tile)) = el.closest(".icon-tile") else {
            return;
        };
        if let Some(name) = tile.get_attribute("data-name") {
            self.selected = name;
            self.has_selection = true;
            self.rebuild_selected();
        }
    }

    pub fn close_detail(&mut self) {
        self.selected = String::new();
        self.has_selection = false;
    }

    pub fn copy_name(&mut self) {
        self.copy(self.selected.clone(), "name");
    }

    pub fn copy_tag(&mut self) {
        self.copy(self.selected_tag.clone(), "<pine-icon> tag");
    }

    pub fn copy_macro(&mut self) {
        self.copy(self.selected_macro.clone(), "icon! macro");
    }
}

impl IconsPage {
    /// Re-filter the catalogue against the query + variant, cap the
    /// rendered set, and refresh the count label. Not a handler.
    fn recompute(&mut self) {
        let all = if self.variant == "filled" {
            icons::FILLED
        } else {
            icons::OUTLINE
        };
        let q = self.query.trim().to_lowercase();
        let mut total = 0usize;
        let mut results = Vec::with_capacity(PAGE);
        for name in all
            .split('\n')
            .filter(|n| !n.is_empty() && (q.is_empty() || n.contains(q.as_str())))
        {
            total += 1;
            if results.len() < PAGE {
                results.push(name.to_string());
            }
        }
        self.count_label = if total > results.len() {
            format!("Showing {} of {} icons", results.len(), total)
        } else {
            format!("{total} icons")
        };
        self.results = results;
    }

    /// Recompute the copy-ready snippets for the selected icon.
    fn rebuild_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let variant_attr = if self.variant == "filled" {
            " variant=\"filled\""
        } else {
            ""
        };
        self.selected_tag = format!(
            "<pine-icon name=\"{}\"{} size=\"{}\"></pine-icon>",
            self.selected, variant_attr, self.size
        );
        self.selected_macro = format!("icon!({} / \"{}\")", self.variant, self.selected);
    }

    /// Write `text` to the clipboard and show a confirmation.
    fn copy(&mut self, text: String, label: &str) {
        if let Some(win) = web_sys::window() {
            let _ = win.navigator().clipboard().write_text(&text);
        }
        self.copied = format!("Copied {label} to clipboard.");
    }
}

impl RouteComponent for IconsPage {}
