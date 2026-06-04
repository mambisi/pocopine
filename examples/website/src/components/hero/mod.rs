//! Landing hero — mascot, wordmark, tagline, and route CTAs.
//! Stateless: the CTAs are plain `pp-route` links, so the hero owns
//! no state and injects nothing.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Hero.poco", style = "hero.css", role = "panel")]
pub struct Hero {}

#[handlers]
impl Hero {}
