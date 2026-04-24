use pine::{PineAvatarFallback, PineAvatarImage, PineAvatarRoot};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "AvatarDemo.poco",
    style = "avatar.css",
    role = "panel",
    // RFC 049 — Avatar is exactly Image + Fallback.
    uses = [PineAvatarRoot, PineAvatarImage, PineAvatarFallback]
)]
pub struct AvatarDemo {}

#[handlers]
impl AvatarDemo {}
