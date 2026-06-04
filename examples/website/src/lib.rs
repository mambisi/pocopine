//! pocopine's own website — the canonical full-stack showcase.
//!
//! `WebsiteApp` is the **app shell**: it owns the site-wide command
//! palette + theme, hosts the router `<pp-outlet>`, and publishes a
//! `WEBSITE_APP` handle into its scope so any descendant (the header,
//! a CTA) can inject it to drive the palette/theme. Routes mount into
//! the outlet:
//!   /                     → Landing (rebranded marketing page)
//!   /components           → ComponentsIndex (Phase 2)
//!   /components/:name      → ComponentPage (Phase 2)
//!   /docs/*slug            → DocPage (Phase 3)

pub mod components;

/// Docs content rendered from `docs/` at build time (see `build.rs`).
pub mod docs_data {
    include!(concat!(env!("OUT_DIR"), "/docs_data.rs"));
}

/// Snippets pre-highlighted by syntect at build time (see `build.rs`).
pub mod gen_code {
    include!(concat!(env!("OUT_DIR"), "/gen_code.rs"));
}

use pocopine::prelude::*;
use pocopine::{inject_key, navigate, provide};
use serde::{Deserialize, Serialize};

use components::showcase::{
    AccordionDemo, AlertDialogDemo, AnimationDemo, AspectRatioDemo, AvatarDemo, Basics, ButtonDemo,
    CalendarDemo, CmdPopoverDemo, CollapsibleDemo, ComboboxDemo, CommandDemo, ContextMenuDemo,
    DatePickerDemo, DateRangePickerDemo, DatetimeFieldsDemo, DialogDemo, DropdownMenuDemo,
    FieldDemo, FieldsetDemo, FormDemo, HoverCardDemo, InputDemo, OtpDemo, PinCardDemo, PopoverDemo,
    RadioGroupDemo, RangeCalendarDemo, ScrollAreaDemo, SelectDemo, SignupDemo, SliderDemo,
    SplitterDemo, StressDemo, SwitchCheckboxDemo, TabsDemo, TagsInputDemo, TagsMentionsDemo,
    TagsSkillsDemo, TextDemo, ToggleDemo, ToolbarDemo, TooltipDemo, TreeDemo,
};
use components::{
    ChartsPage, ComponentPage, ComponentsIndex, DocPage, EasingPlayground, Hero, IconsPage,
    InstallCmd, IssueFlowDemo, Landing, MotionPage, SecureSection, ShowcaseCard, SiteHeader,
    StackFlow, StackShowcase, Tutorial,
};

/// One searchable entry in the ⌘K palette — a doc page, a component,
/// or a top-level page. `value` is the route to navigate to (and what
/// pine-command matches against, alongside `label`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SearchEntry {
    pub label: String,
    pub value: String,
    pub kind: String,
}

#[derive(Serialize, Deserialize)]
#[component(template = "WebsiteApp.poco")]
pub struct WebsiteApp {
    /// Command-palette visibility (bound via `pp-model:open`).
    pub open: bool,
    /// `"light"` | `"dark"` — mirrored onto `<html data-theme>`.
    pub theme: String,
    /// Searchable index (docs + components + pages) for the palette.
    pub search: Vec<SearchEntry>,
}

impl Default for WebsiteApp {
    fn default() -> Self {
        Self {
            open: false,
            theme: "light".into(),
            search: Vec::new(),
        }
    }
}

inject_key!(pub WEBSITE_APP: Handle<WebsiteApp>);

/// localStorage key for the persisted theme (shared with the pre-paint
/// restore script in `index.html`).
const THEME_KEY: &str = "pocopine-theme";

#[handlers]
impl WebsiteApp {
    pub fn on_setup(&mut self) {
        provide(&WEBSITE_APP, this::<Self>());
        // Restore the saved theme across reloads / deep-links. The
        // inline script in index.html already applied it before paint;
        // this syncs our own state (and re-applies on a remount).
        if let Ok(Some(saved)) = LocalStorage::<String>::new(THEME_KEY).get() {
            if saved == "dark" || saved == "light" {
                self.theme = saved;
            }
        }
        self.apply_theme();
        self.search = build_search_index();
    }

    pub fn open_palette(&mut self) {
        self.open = true;
    }

    pub fn toggle_theme(&mut self) {
        self.theme = if self.theme == "dark" {
            "light".into()
        } else {
            "dark".into()
        };
        self.apply_theme();
        let _ = LocalStorage::<String>::new(THEME_KEY).set(&self.theme);
    }

    pub fn open_github(&mut self) {
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url("https://github.com/mambisi/pocopine");
        }
    }

    // ─── command-palette ──────────────────────────────────────────
    /// A palette result was selected. pine-command emits the item's
    /// `value` (the route) as the event detail; navigate to it.
    pub fn on_command(&mut self, ev: web_sys::CustomEvent) {
        self.open = false;
        if let Some(target) = ev.detail().as_string() {
            if !target.is_empty() {
                navigate(&target);
            }
        }
    }
    pub fn cmd_toggle_theme(&mut self) {
        self.open = false;
        self.toggle_theme();
    }
    pub fn cmd_github(&mut self) {
        self.open = false;
        self.open_github();
    }
}

impl WebsiteApp {
    /// Mirror `self.theme` onto `<html data-theme>` (not a handler).
    fn apply_theme(&self) {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(root) = doc.document_element() {
                let _ = root.set_attribute("data-theme", &self.theme);
            }
        }
    }
}

/// Build the ⌘K search index from the generated docs nav + the
/// component catalogue — every doc page and component is searchable
/// and navigable. `value` is the route (also matched by pine-command).
fn build_search_index() -> Vec<SearchEntry> {
    use std::collections::HashSet;
    let mut out: Vec<SearchEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |out: &mut Vec<SearchEntry>,
                seen: &mut HashSet<String>,
                label: String,
                value: String,
                kind: &str| {
        if seen.insert(value.clone()) {
            out.push(SearchEntry {
                label,
                value,
                kind: kind.into(),
            });
        }
    };
    push(&mut out, &mut seen, "Home".into(), "/".into(), "Page");
    push(
        &mut out,
        &mut seen,
        "Components".into(),
        "/components".into(),
        "Page",
    );
    for c in components::component_meta::COMPONENTS {
        push(
            &mut out,
            &mut seen,
            c.title.into(),
            format!("/components/{}", c.slug),
            "Component",
        );
    }
    for n in docs_data::NAV {
        push(
            &mut out,
            &mut seen,
            n.title.into(),
            format!("/docs/{}", n.slug),
            n.group,
        );
    }
    out
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    pine_charts::register_all();
    pine_icons::register_icons![
        "search",
        "moon",
        "sun",
        "chevron-down",
        "chevron-right",
        "check",
        "brand-github",
        "x",
        "arrow-right",
        "book",
        "components",
    ];
    App::new()
        .register::<WebsiteApp>()
        .register::<SiteHeader>()
        .register::<pine_icons::PineIcon>()
        .register::<Hero>()
        .register::<Tutorial>()
        .register::<InstallCmd>()
        .register::<StackShowcase>()
        .register::<StackFlow>()
        .register::<IssueFlowDemo>()
        .register::<SecureSection>()
        .register::<ShowcaseCard>()
        // All primitive demos — reused as live previews on the
        // component reference pages.
        .register::<Basics>()
        .register::<AspectRatioDemo>()
        .register::<ToolbarDemo>()
        .register::<ButtonDemo>()
        .register::<CalendarDemo>()
        .register::<RangeCalendarDemo>()
        .register::<DatePickerDemo>()
        .register::<DateRangePickerDemo>()
        .register::<DatetimeFieldsDemo>()
        .register::<DialogDemo>()
        .register::<AlertDialogDemo>()
        .register::<PopoverDemo>()
        .register::<DropdownMenuDemo>()
        .register::<ContextMenuDemo>()
        .register::<AvatarDemo>()
        .register::<CollapsibleDemo>()
        .register::<AccordionDemo>()
        .register::<TabsDemo>()
        .register::<HoverCardDemo>()
        .register::<TooltipDemo>()
        .register::<RadioGroupDemo>()
        .register::<ToggleDemo>()
        .register::<SwitchCheckboxDemo>()
        .register::<OtpDemo>()
        .register::<PinCardDemo>()
        .register::<InputDemo>()
        .register::<FieldDemo>()
        .register::<FieldsetDemo>()
        .register::<FormDemo>()
        .register::<SignupDemo>()
        .register::<SelectDemo>()
        .register::<ComboboxDemo>()
        .register::<CommandDemo>()
        .register::<SliderDemo>()
        .register::<ScrollAreaDemo>()
        .register::<SplitterDemo>()
        .register::<TreeDemo>()
        .register::<TagsInputDemo>()
        .register::<TagsSkillsDemo>()
        .register::<TagsMentionsDemo>()
        .register::<TextDemo>()
        .register::<CmdPopoverDemo>()
        .register::<StressDemo>()
        .register::<AnimationDemo>()
        .register::<EasingPlayground>()
        // Routes.
        .route::<Landing>("/")
        .route::<ComponentsIndex>("/components")
        .route::<IconsPage>("/components/icons")
        .route::<ComponentPage>("/components/:name")
        .route::<ChartsPage>("/charts/:name")
        .route::<MotionPage>("/motion/:name")
        .route::<DocPage>("/docs/*slug")
        .run();
}
