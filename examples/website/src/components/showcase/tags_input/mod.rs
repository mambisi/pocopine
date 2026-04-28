use pine::{
    PineButton, PineTagsInputClear, PineTagsInputInput, PineTagsInputItem, PineTagsInputItemDelete,
    PineTagsInputItemText, PineTagsInputRoot,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "TagsInputDemo.poco",
    style = "tags_input.css",
    role = "panel",
    // RFC 049 — TagsInput Root is loose (author markup is
    // expected) but its Item owns Text + Delete siblings.
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
pub struct TagsInputDemo {
    pub tags: Vec<String>,
}

#[handlers]
impl TagsInputDemo {
    pub fn on_mount(&mut self) {
        self.tags = vec!["rust".into(), "wasm".into(), "pocopine".into()];
    }
    pub fn shuffle(&mut self) {
        rotate(&mut self.tags);
    }
}

fn rotate(v: &mut Vec<String>) {
    if v.len() < 2 {
        return;
    }
    let head = v.remove(0);
    v.push(head);
}
