use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "OtpDemo.poco", style = "otp.css", role = "panel")]
pub struct OtpDemo {
    pub otp_code: String,
}

#[handlers]
impl OtpDemo {}
