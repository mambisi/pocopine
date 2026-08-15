//! RFC-116 — `template_inline = "..."` was removed. The diagnostic names the
//! replacement, because the rewrite changes shape (a string literal becomes
//! HTML tokens) and is not obvious from the key name alone.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "poco-removed-key",
    template_inline = r#"<div><span pp-text="count"></span></div>"#
)]
struct PocoRemovedKey {
    count: i32,
}

#[handlers]
impl PocoRemovedKey {}

fn main() {}
