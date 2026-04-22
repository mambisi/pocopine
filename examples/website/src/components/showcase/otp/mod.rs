use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "OtpDemo.poco", role = "panel")]
pub struct OtpDemo {
    pub code: String,
}

#[handlers]
impl OtpDemo {}
