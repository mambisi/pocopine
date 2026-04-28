use pine::{
    PineButton, PineTagsInputClear, PineTagsInputInput, PineTagsInputItem, PineTagsInputItemDelete,
    PineTagsInputItemText, PineTagsInputRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TagsSkillsDemo.poco",
    style = "../tags_input/tags_input.css",
    role = "panel",
    // RFC 049 — styling variant of TagsInput.
    uses = [
        PineTagsInputRoot,
        PineTagsInputItem,
        PineTagsInputItemText,
        PineTagsInputItemDelete,
        PineTagsInputInput,
        PineTagsInputClear,
        PineButton,
    ]
)]
pub struct TagsSkillsDemo {
    pub skills: Vec<String>,
}

#[handlers]
impl TagsSkillsDemo {
    pub fn on_mount(&mut self) {
        self.skills = vec!["typescript".into(), "css".into()];
    }
    pub fn shuffle(&mut self) {
        if self.skills.len() < 2 {
            return;
        }
        let head = self.skills.remove(0);
        self.skills.push(head);
    }
}
