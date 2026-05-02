//! Router fallback page.
//!
//! Split builds source the static descriptor artifact from
//! `examples/hn/routes/not_found`. This in-crate component remains as
//! the current `app!` manifest anchor until RFC 067 route-crate build
//! integration can replace route component paths directly.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}
