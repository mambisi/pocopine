use pine::{
    PineAvatarFallback, PineAvatarImage, PineAvatarRoot, PineButton, PineTagsInputClear,
    PineTagsInputInput, PineTagsInputItem, PineTagsInputItemDelete, PineTagsInputItemText,
    PineTagsInputRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TagsMentionsDemo.poco",
    style = "../tags_input/tags_input.css",
    role = "panel",
    // RFC 049 — TagsInput with per-chip Avatars: each Item hosts
    // an Avatar compound plus the standard Text + Delete parts.
    uses = [
        PineTagsInputRoot,
        PineTagsInputItem,
        PineTagsInputItemText,
        PineTagsInputItemDelete,
        PineTagsInputInput,
        PineTagsInputClear,
        PineAvatarRoot,
        PineAvatarImage,
        PineAvatarFallback,
        PineButton,
    ]
)]
pub struct TagsMentionsDemo {
    pub mentions: Vec<String>,
}

#[handlers]
impl TagsMentionsDemo {
    pub fn on_mount(&mut self) {
        self.mentions = vec!["ada".into(), "grace".into(), "linus".into()];
    }
    pub fn shuffle(&mut self) {
        if self.mentions.len() < 2 {
            return;
        }
        let head = self.mentions.remove(0);
        self.mentions.push(head);
    }
}
