//! Template-path validation — a template expression rooted at a name
//! that is neither a field, an explicit flatten leaf, a `#[computed]`,
//! nor locally bound fails to compile (missing
//! `__poc_bindable_countt` marker; rustc's did-you-mean points at the
//! real field's marker).
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "tp-unknown-field",
    template_inline = r#"<div><span pp-text="countt"></span></div>"#
)]
struct TpUnknownField {
    count: i32,
}

#[handlers]
impl TpUnknownField {}

fn main() {}
