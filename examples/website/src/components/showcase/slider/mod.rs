use pine::{PineSliderRange, PineSliderRoot, PineSliderThumb, PineSliderTrack};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "SliderDemo.poco",
    style = "slider.css",
    role = "panel",
    // RFC 049 — Slider Root allows Track + Thumb children; Track
    // allows Range inside.
    uses = [
        PineSliderRoot,
        PineSliderTrack,
        PineSliderRange,
        PineSliderThumb,
    ]
)]
pub struct SliderDemo {
    pub volume: f64,
}

#[handlers]
impl SliderDemo {
    pub fn on_mount(&mut self) {
        self.volume = 40.0;
    }
}
