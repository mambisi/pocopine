use pine::{
    PineSelectContent, PineSelectItem, PineSelectPortal, PineSelectRoot, PineSelectSeparator,
    PineSelectTrigger, PineSelectValue,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "SelectDemo.poco",
    style = "select.css",
    role = "panel",
    // RFC 049 — Select Root = Trigger + Portal > Content. Content
    // only accepts Items and Separators; Trigger hosts a Value
    // view (plus any chevron markup).
    uses = [
        PineSelectRoot,
        PineSelectTrigger,
        PineSelectValue,
        PineSelectPortal,
        PineSelectContent,
        PineSelectItem,
        PineSelectSeparator,
    ]
)]
pub struct SelectDemo {
    pub city: String,
}

#[handlers]
impl SelectDemo {}
