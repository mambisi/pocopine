use pine::PineOtpField;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Composite demo showing two `PineOtpField`s driving a single card
/// form. Exercises two OTP lifetimes side-by-side (+ masked PIN +
/// completion state) so the delete / retype flow gets real mileage
/// beyond the standalone OTP demo.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PinCardDemo.poco",
    style = "pin_card.css",
    role = "panel",
    // RFC 049 — single primitive used twice.
    uses = [PineOtpField]
)]
pub struct PinCardDemo {
    pub card_number: String,
    pub pin: String,
}

#[handlers]
impl PinCardDemo {
    #[computed]
    fn complete(card_number: &str, pin: &str) -> bool {
        card_number.chars().count() == 16 && pin.chars().count() == 4
    }
}
