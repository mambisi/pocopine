//! pocopine's own website. A composition-only root that drops
//! in a `Hero`, a `Tutorial`, and a `Showcase`. The showcase is
//! itself a thin composer of 31 per-primitive demo components
//! that each own their own state — no shared blob. Same layout
//! the `hn` example uses.

pub mod components;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use components::showcase::{
    AccordionDemo, AlertDialogDemo, AspectRatioDemo, AvatarDemo, Basics, ButtonDemo,
    CmdPopoverDemo, CollapsibleDemo, ComboboxDemo, CommandDemo, ContextMenuDemo, DialogDemo,
    DropdownMenuDemo, HoverCardDemo, OtpDemo, PopoverDemo, RadioGroupDemo, ScrollAreaDemo,
    SelectDemo, SliderDemo, SplitterDemo, StressDemo, SwitchCheckboxDemo, TabsDemo,
    TagsInputDemo, TagsMentionsDemo, TagsSkillsDemo, ToggleDemo, ToolbarDemo, TooltipDemo,
    TreeDemo,
};
use components::{Hero, Showcase, ShowcaseCard, Tutorial};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "WebsiteApp.poco")]
pub struct WebsiteApp {}

#[handlers]
impl WebsiteApp {}

#[wasm_bindgen(start)]
pub fn main() {
    pine::register_all();
    App::new()
        .register::<WebsiteApp>()
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
        .register::<CmdPopoverDemo>()
        .register::<StressDemo>()
        .run();
}
