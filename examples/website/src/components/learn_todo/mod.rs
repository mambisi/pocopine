//! `/learn` — an Apple-SwiftUI-tutorials-style guided build of a small
//! Todo app: a sticky section sidebar (scroll-spy), chapters of prose +
//! code, and a sticky live preview of the finished app. Code blocks are
//! bound via `pp-text` (textContent), so snippets need no HTML escaping.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "LearnTodo.poco", style = "learn_todo.css")]
pub struct LearnTodo {
    /// Active chapter (1-based) — driven by scroll via pp-intersect.
    pub active: u32,
    /// Short label shown on the live preview for the active chapter.
    pub stage_note: String,
    pub code1: String,
    pub code2: String,
    pub code3: String,
    pub code4: String,
    pub code5: String,
    pub code6: String,
    pub code7: String,
}

#[handlers]
impl LearnTodo {
    pub fn on_setup(&mut self) {
        self.active = 1;
        self.stage_note = "the model".into();
        self.code1 = "#[derive(Clone, Serialize, Deserialize)]\nstruct Todo {\n    id: u32,\n    text: String,\n    done: bool,\n}".into();
        self.code2 = "#[derive(Default, Serialize, Deserialize)]\n#[component(template = \"Todo.poco\")]\npub struct TodoApp {\n    items: Vec<Todo>,\n    draft: String,\n}".into();
        self.code3 = "<!-- Todo.poco -->\n<ul pp-on:click=\"on_click\">\n  <template pp-for=\"t in items\" pp-key=\"t.id\">\n    <li :data-done=\"t.done\">\n      <span pp-text=\"t.text\"></span>\n    </li>\n  </template>\n</ul>".into();
        self.code4 = "#[handlers]\nimpl TodoApp {\n    pub fn add(&mut self) {\n        let text = self.draft.trim();\n        if text.is_empty() { return; }\n        self.items.push(Todo::new(text));\n        self.draft.clear();\n    }\n}".into();
        self.code5 = "// toggle / remove, delegated from the list click\npub fn on_click(&mut self, ev: MouseEvent) {\n    let id = data_id(&ev);\n    for t in &mut self.items {\n        if t.id == id { t.done = !t.done; }\n    }\n}".into();
        self.code6 = "#[pocopine::server]\nasync fn save(items: Vec<Todo>) -> ServerResult<()> {\n    db().replace_all(items).await?;\n    Ok(())\n}\n// the client calls save(..).await like a normal async fn".into();
        self.code7 = "$ pocopine build --release\n$ pocopine deploy\n  ✓ web process → railway\n  → https://todo.up.railway.app".into();
    }

    pub fn enter1(&mut self) {
        self.active = 1;
        self.stage_note = "the model".into();
    }
    pub fn enter2(&mut self) {
        self.active = 2;
        self.stage_note = "the component".into();
    }
    pub fn enter3(&mut self) {
        self.active = 3;
        self.stage_note = "rendering the list".into();
    }
    pub fn enter4(&mut self) {
        self.active = 4;
        self.stage_note = "adding tasks".into();
    }
    pub fn enter5(&mut self) {
        self.active = 5;
        self.stage_note = "toggle & remove".into();
    }
    pub fn enter6(&mut self) {
        self.active = 6;
        self.stage_note = "saving to the server".into();
    }
    pub fn enter7(&mut self) {
        self.active = 7;
        self.stage_note = "shipped".into();
    }
}

impl RouteComponent for LearnTodo {}
