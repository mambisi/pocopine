//! Template-path validation — compile-pass: `unchecked_paths =
//! "true"` is the escape hatch; unknown roots fall back to the
//! runtime warn path.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "tp-unchecked",
    template_inline = r#"<span pp-text="not_a_field"></span>"#,
    unchecked_paths = "true"
)]
struct TpUnchecked {
    count: i32,
}

#[handlers]
impl TpUnchecked {}

fn main() {}
