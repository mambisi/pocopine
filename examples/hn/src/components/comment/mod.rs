//! `<hn-comment>` — one node in the recursive comment tree. Renders
//! its own meta + body, then iterates `comment.children` as sibling
//! `<hn-comment>` instances.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Client-side display shape for a single comment. Computed once
/// when the server response lands (age formatted, dead / deleted
/// nodes dropped) so the template stays declarative.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub age: String,
    /// HN's text field — already HTML. Injected via `pp-html`.
    pub body: String,
    pub children: Vec<Comment>,
}

#[derive(Default, Serialize, Deserialize)]
#[component(uses = [HnComment])]
pub struct HnComment {
    #[prop]
    pub comment: Comment,
}

#[handlers]
impl HnComment {}
