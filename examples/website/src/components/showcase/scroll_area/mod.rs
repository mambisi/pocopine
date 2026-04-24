use pine::{
    PineRadioGroupIndicator, PineRadioGroupItem, PineRadioGroupRoot, PineScrollAreaCorner,
    PineScrollAreaRoot, PineScrollAreaScrollbar, PineScrollAreaThumb, PineScrollAreaViewport,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ScrollAreaDemo.poco",
    style = "scroll_area.css",
    role = "panel",
    // RFC 049 — ScrollArea Root takes Viewport, Scrollbars, and
    // Corner; each Scrollbar's only child is Thumb. A RadioGroup
    // here lets the reader toggle `type` live.
    uses = [
        PineScrollAreaRoot,
        PineScrollAreaViewport,
        PineScrollAreaScrollbar,
        PineScrollAreaThumb,
        PineScrollAreaCorner,
        PineRadioGroupRoot,
        PineRadioGroupItem,
        PineRadioGroupIndicator,
    ]
)]
pub struct ScrollAreaDemo {
    pub scroll_type: String,
}

#[handlers]
impl ScrollAreaDemo {
    pub fn on_mount(&mut self) {
        self.scroll_type = "hover".into();
    }
}
