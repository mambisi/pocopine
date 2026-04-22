use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "AvatarDemo.poco", style = "avatar.css", role = "panel")]
pub struct AvatarDemo {}

#[handlers]
impl AvatarDemo {}
