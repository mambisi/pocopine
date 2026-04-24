use pine::{PineContextMenuContent, PineContextMenuItem, PineContextMenuSeparator};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ContextMenuDemo.poco",
    style = "context_menu.css",
    role = "panel",
    // RFC 049 reference migration: list the one typed parent
    // (`<pine-context-menu-content>` declared `#[slot(default,
    // only = [...])]`) and its accepted children. Other
    // pocopine components referenced in the template
    // (`<pine-context-menu-root>` / -trigger / -portal) don't
    // have typed slots, so they're intentionally NOT listed
    // here — including them would make the scan expect a
    // <ParentType>DefaultChild trait that those primitives
    // don't emit.
    uses = [
        PineContextMenuContent,
        PineContextMenuItem,
        PineContextMenuSeparator,
    ]
)]
pub struct ContextMenuDemo {}

#[handlers]
impl ContextMenuDemo {}
