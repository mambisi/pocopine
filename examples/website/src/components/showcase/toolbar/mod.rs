use pine::{
    PineToggleGroupItem, PineToggleGroupRoot, PineToolbarButton, PineToolbarLink, PineToolbarRoot,
    PineToolbarSeparator,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ToolbarDemo.poco",
    style = "toolbar.css",
    role = "panel",
    // RFC 049 — Toolbar Root is loose; its documented parts plus a
    // nested ToggleGroup illustrate compound reuse.
    uses = [
        PineToolbarRoot,
        PineToolbarButton,
        PineToolbarSeparator,
        PineToolbarLink,
        PineToggleGroupRoot,
        PineToggleGroupItem,
    ]
)]
pub struct ToolbarDemo {}

#[handlers]
impl ToolbarDemo {}
