use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = r#"<pp-component :is="active"></pp-component>"#)]
struct DcStringRefHost {
    active: String,
}

#[handlers]
impl DcStringRefHost {}

fn main() {}
