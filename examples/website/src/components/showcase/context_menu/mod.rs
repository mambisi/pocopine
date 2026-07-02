use pine::{
    PineContextMenuContent, PineContextMenuItem, PineContextMenuPortal, PineContextMenuRoot,
    PineContextMenuSeparator, PineContextMenuTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ContextMenuDemo.poco",
    style = "context_menu.css",
    role = "panel",
    // RFC 060 — every custom-element tag in the template must
    // appear in `uses`. Root/Trigger/Portal don't expose typed
    // slot contracts (RFC 049 §4.3 still treats their children
    // as loose) but Tier 2 requires them as registry entries.
    uses = [
        PineContextMenuRoot,
        PineContextMenuTrigger,
        PineContextMenuPortal,
        PineContextMenuContent,
        PineContextMenuItem,
        PineContextMenuSeparator,
    ]
)]
pub struct ContextMenuDemo {}

#[handlers]
impl ContextMenuDemo {
    /// Showcase no-op — the demo exercises the menu interaction
    /// (open/dismiss/keyboard), not the action itself.
    pub fn action_bump(&mut self) {}

    /// Showcase no-op — see [`Self::action_bump`].
    pub fn action_reset(&mut self) {}
}
