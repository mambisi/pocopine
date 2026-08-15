use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! {<pp-component :is="'dc-literal-child'"></pp-component>})]
struct DcLiteralRefHost {}

#[handlers]
impl DcLiteralRefHost {}

fn main() {}
