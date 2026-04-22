//! pocopine's own website. The app root (`WebsiteApp`) is also
//! the canonical root-provider — it owns the site-wide command
//! palette state + theme, publishes a `WEBSITE_APP` handle into
//! its scope, and lets any descendant (the header, a hero CTA,
//! a single demo card) inject that handle to drive the palette.
//! The palette DOM itself lives in `WebsiteApp.poco` so state
//! and view sit together; `SiteHeader` is a thin top-bar view
//! that only forwards clicks up to the app root.

pub mod components;

use pocopine::prelude::*;
use pocopine::{inject_key, provide};
use serde::{Deserialize, Serialize};

use components::showcase::{
    AccordionDemo, AlertDialogDemo, AspectRatioDemo, AvatarDemo, Basics, ButtonDemo,
    CalendarDemo, CmdPopoverDemo, CollapsibleDemo, ComboboxDemo, CommandDemo, ContextMenuDemo,
    DatePickerDemo, DialogDemo, DropdownMenuDemo, FieldDemo, FieldsetDemo, FormDemo, HoverCardDemo, InputDemo,
    OtpDemo, PopoverDemo, RadioGroupDemo, RangeCalendarDemo,
    ScrollAreaDemo, SelectDemo, SignupDemo, SliderDemo, SplitterDemo, StressDemo,
    SwitchCheckboxDemo, TabsDemo,
    TagsInputDemo, TagsMentionsDemo, TagsSkillsDemo, TextDemo, ToggleDemo, ToolbarDemo,
    TooltipDemo, TreeDemo,
};
use components::{Hero, Showcase, ShowcaseCard, SiteHeader, Tutorial};

#[derive(Serialize, Deserialize)]
#[component(template = "WebsiteApp.poco")]
pub struct WebsiteApp {
    pub open: bool,
    pub theme: String,
    pub last: String,
}

impl Default for WebsiteApp {
    fn default() -> Self {
        Self {
            open: false,
            theme: "light".into(),
            last: String::new(),
        }
    }
}

inject_key!(pub WEBSITE_APP: Handle<WebsiteApp>);

#[handlers]
impl WebsiteApp {
    pub fn on_setup(&mut self) {
        provide(&WEBSITE_APP, this::<Self>());
    }

    pub fn open_palette(&mut self) { self.open = true; }

    pub fn toggle_theme(&mut self) {
        self.theme = if self.theme == "dark" { "light".into() } else { "dark".into() };
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(root) = doc.document_element() {
                let _ = root.set_attribute("data-theme", &self.theme);
            }
        }
    }

    pub fn go_hero(&mut self) {
        self.last = "Hero".into();
        self.open = false;
        Self::scroll_to("hero");
    }
    pub fn go_tutorial(&mut self) {
        self.last = "Install".into();
        self.open = false;
        Self::scroll_to("install");
    }
    pub fn go_showcase(&mut self) {
        self.last = "Showcase".into();
        self.open = false;
        Self::scroll_to("showcase");
    }
    pub fn go_github(&mut self) {
        self.last = "GitHub".into();
        self.open = false;
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url("https://github.com/mambisi/pocopine");
        }
    }
    pub fn cmd_toggle_theme(&mut self) {
        self.last = "Toggle theme".into();
        self.open = false;
        self.toggle_theme();
    }

    fn scroll_to(id: &str) {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(el) = doc.get_element_by_id(id) {
                el.scroll_into_view();
            }
        }
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    // Pull in the icons the header + demos actually reference.
    // Any icon not in this list stays out of the WASM binary.
    pine_icons::register_icons![
        "search",
        "moon",
        "sun",
        "chevron-down",
        "chevron-right",
        "check",
        "x",
    ];
    App::new()
        .register::<WebsiteApp>()
        .register::<SiteHeader>()
        .register::<pine_icons::PineIcon>()
        .register::<Hero>()
        .register::<Tutorial>()
        .register::<Showcase>()
        .register::<ShowcaseCard>()
        // All 31 primitive demos. Every one is a focused
        // `#[component]` owning only its own reactive state.
        .register::<Basics>()
        .register::<AspectRatioDemo>()
        .register::<ToolbarDemo>()
        .register::<ButtonDemo>()
        .register::<CalendarDemo>()
        .register::<RangeCalendarDemo>()
        .register::<DatePickerDemo>()
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
        .run();
}
