//! Static 404 route crate.
//!
//! This is the first HN route using the RFC 067 route-crate ABI entry
//! point. The split builder reads this crate's static `.poco` template
//! and emits descriptor JS instead of a route wasm chunk.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

pocopine::route! {
    id: "not_found",
    path: "*",
    component: NotFound,
}

