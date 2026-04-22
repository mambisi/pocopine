//! Landing hero — mascot, wordmark, tagline, CTAs. Stateless.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Hero.poco", role = "panel")]
pub struct Hero {}

#[handlers]
impl Hero {}
