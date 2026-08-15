//! RFC-116 — the inline `poco!` form must reach RFC-111 template-path
//! validation exactly like a file or string template does. A root that is
//! neither a field nor a `#[computed]` fails to compile, and the diagnostic
//! points into the HTML body.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "poco-unknown-field", template = poco! {
    <div><span pp-text="countt"></span></div>
})]
struct PocoUnknownField {
    count: i32,
}

#[handlers]
impl PocoUnknownField {}

fn main() {}
