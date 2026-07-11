use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = "<pp-component></pp-component>")]
struct DcMissingIs {}

#[handlers]
impl DcMissingIs {}

fn main() {}
