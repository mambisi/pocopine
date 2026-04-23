use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Composite demo showing two `PineOtpField`s driving a single card
/// form. Exercises two OTP lifetimes side-by-side (+ masked PIN +
/// completion state) so the delete / retype flow gets real mileage
/// beyond the standalone OTP demo.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinCardDemo.poco", style = "pin_card.css", role = "panel")]
pub struct PinCardDemo {
    pub card_number: String,
    pub pin: String,
    /// Derived mirror of `card_number.len() == 16 && pin.len() == 4`.
    /// Kept as a plain bool so the template can `pp-show="complete"`
    /// without reaching through the whole-string predicate in expr.
    pub complete: bool,
}

#[handlers]
impl PinCardDemo {
    #[watch(card_number)]
    fn on_card_change(&mut self, _new: String, _prev: Option<String>) {
        self.recompute_complete();
    }

    #[watch(pin)]
    fn on_pin_change(&mut self, _new: String, _prev: Option<String>) {
        self.recompute_complete();
    }
}

impl PinCardDemo {
    fn recompute_complete(&mut self) {
        self.complete = self.card_number.chars().count() == 16 && self.pin.chars().count() == 4;
    }
}
