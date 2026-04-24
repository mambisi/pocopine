use pine::{
    PineAvatarFallback, PineAvatarImage, PineAvatarRoot, PineHoverCardContent,
    PineHoverCardPortal, PineHoverCardRoot, PineHoverCardTrigger,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "HoverCardDemo.poco",
    style = "hover_card.css",
    role = "panel",
    // RFC 049 — HoverCard Root = Trigger + Portal > Content.
    // Avatar primitives appear inside Content as author markup.
    uses = [
        PineHoverCardRoot,
        PineHoverCardTrigger,
        PineHoverCardPortal,
        PineHoverCardContent,
        PineAvatarRoot,
        PineAvatarImage,
        PineAvatarFallback,
    ]
)]
pub struct HoverCardDemo {}

#[handlers]
impl HoverCardDemo {}
