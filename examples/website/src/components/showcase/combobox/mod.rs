use pine::{
    PineComboboxContent, PineComboboxEmpty, PineComboboxInput, PineComboboxItem,
    PineComboboxPortal, PineComboboxRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ComboboxDemo.poco",
    style = "combobox.css",
    role = "panel",
    // RFC 049 — Combobox Root = Input + Portal > Content; Content
    // hosts Items + an Empty fallback.
    uses = [
        PineComboboxRoot,
        PineComboboxInput,
        PineComboboxPortal,
        PineComboboxContent,
        PineComboboxItem,
        PineComboboxEmpty,
    ]
)]
pub struct ComboboxDemo {
    pub framework: String,
}

#[handlers]
impl ComboboxDemo {}
