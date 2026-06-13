//! RFC-097 §3.2 — a field whose name collides with a `Handle` inherent
//! method (`update`) must be rejected: the generated accessor would be
//! silently shadowed by the inherent method.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "fh-reserved", template_inline = r#"<div></div>"#)]
struct Reserved {
    update: u32,
}

fn main() {}
