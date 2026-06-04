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
    ComponentPage, ComponentsIndex, DocPage, Hero, InstallCmd, Landing, SecureSection,
    ShowcaseCard, SiteHeader, StackFlow, StackShowcase, Tutorial,
};

#[derive(Serialize, Deserialize)]
#[component(template = "WebsiteApp.poco")]
pub struct WebsiteApp {
    /// Command-palette visibility (bound via `pp-model:open`).
    pub open: bool,
    /// `"light"` | `"dark"` — mirrored onto `<html data-theme>`.
    pub theme: String,
}

impl Default for WebsiteApp {
    fn default() -> Self {
        Self {
            open: false,
            theme: "light".into(),
        }
    }
}

inject_key!(pub WEBSITE_APP: Handle<WebsiteApp>);

#[handlers]
impl WebsiteApp {
    pub fn on_setup(&mut self) {
        provide(&WEBSITE_APP, this::<Self>());
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
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(root) = doc.document_element() {
                let _ = root.set_attribute("data-theme", &self.theme);
            }
        }
    }

    pub fn open_github(&mut self) {
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url("https://github.com/mambisi/pocopine");
        }
    }

    // ─── command-palette navigation ──────────────────────────────
    pub fn nav_home(&mut self) {
        self.open = false;
        navigate("/");
    }
    pub fn nav_components(&mut self) {
        self.open = false;
        navigate("/components");
    }
    pub fn nav_docs(&mut self) {
        self.open = false;
        navigate("/docs/getting-started/introduction");
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

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
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
        // Routes.
        .route::<Landing>("/")
        .route::<ComponentsIndex>("/components")
        .route::<ComponentPage>("/components/:name")
        .route::<DocPage>("/docs/*slug")
        .run();
}
