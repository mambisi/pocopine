//! Router fallback page.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}
