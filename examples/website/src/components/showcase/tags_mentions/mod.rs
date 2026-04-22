use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TagsMentionsDemo.poco", role = "panel")]
pub struct TagsMentionsDemo {
    pub mentions: Vec<String>,
}

#[handlers]
impl TagsMentionsDemo {
    pub fn on_mount(&mut self) {
        self.mentions = vec!["ada".into(), "grace".into(), "linus".into()];
    }
    pub fn shuffle(&mut self) {
        if self.mentions.len() < 2 { return; }
        let head = self.mentions.remove(0);
        self.mentions.push(head);
    }
}
