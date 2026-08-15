use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <pp-component></pp-component> })]
struct DcMissingIs {}

#[handlers]
impl DcMissingIs {}

fn main() {}
