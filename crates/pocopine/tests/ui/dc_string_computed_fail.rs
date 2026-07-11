use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = r#"<pp-component :is="active"></pp-component>"#)]
struct DcStringComputedHost {
    route: String,
}

#[handlers]
impl DcStringComputedHost {
    #[computed]
    fn active(route: &str) -> String {
        match route {
            "general" => "dc-general",
            _ => "",
        }
        .to_string()
    }
}

fn main() {}
