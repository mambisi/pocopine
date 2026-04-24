use pine::PineOtpField;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "OtpDemo.poco",
    style = "otp.css",
    role = "panel",
    // RFC 049 — single-element primitive.
    uses = [PineOtpField]
)]
pub struct OtpDemo {
    pub otp_code: String,
}

#[handlers]
impl OtpDemo {}
