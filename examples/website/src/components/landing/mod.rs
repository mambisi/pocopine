//! Landing route (`/`) — the rebranded marketing page: hero,
//! full-stack feature highlights, install steps, and CTAs into the
//! component reference and the docs. Stateless; composition lives in
//! `Landing.poco`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Landing.poco", style = "landing.css", role = "panel")]
pub struct Landing {}

#[handlers]
impl Landing {}

impl RouteComponent for Landing {}
