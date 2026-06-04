//! A small, fully interactive Todo app — the running example for the
//! `/learn` tutorial and a live preview of what pocopine feels like.
//! Add via the form, toggle/remove via delegated clicks on the list.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Todo {
    pub id: u32,
    pub text: String,
    pub done: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TodoDemo.poco", style = "todo_demo.css")]
pub struct TodoDemo {
    pub items: Vec<Todo>,
    pub draft: String,
    pub next_id: u32,
    pub left: u32,
    /// Tutorial chapter (1–7), bound from the page; gates which
    /// features are revealed so the preview evolves as you scroll.
    /// `0` (the default when used standalone) shows everything.
    #[prop]
    pub stage: u32,
}

#[handlers]
impl TodoDemo {
    pub fn on_setup(&mut self) {
        self.items = vec![
            Todo {
                id: 1,
                text: "Read the pocopine guide".into(),
                done: true,
            },
            Todo {
                id: 2,
                text: "Build a component".into(),
                done: false,
            },
            Todo {
                id: 3,
                text: "Ship it with one command".into(),
                done: false,
            },
        ];
        self.next_id = 4;
        self.recount();
    }

    /// Add the current draft as a new todo (form submit).
    pub fn add(&mut self) {
        let text = self.draft.trim();
        if text.is_empty() {
            return;
        }
        let mut next = self.items.clone();
        next.push(Todo {
            id: self.next_id,
            text: text.to_string(),
            done: false,
        });
        self.items = next; // reassign so the pp-for re-renders
        self.next_id += 1;
        self.draft.clear();
        self.recount();
    }

    /// Toggle or remove an item — delegated from a click on the list.
    pub fn on_list_click(&mut self, ev: web_sys::MouseEvent) {
        use wasm_bindgen::JsCast;
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let Ok(Some(node)) = el.closest("[data-action]") else {
            return;
        };
        let action = node.get_attribute("data-action").unwrap_or_default();
        let id: u32 = node
            .get_attribute("data-id")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        match action.as_str() {
            "toggle" => {
                self.items = self
                    .items
                    .iter()
                    .map(|t| {
                        let mut t = t.clone();
                        if t.id == id {
                            t.done = !t.done;
                        }
                        t
                    })
                    .collect();
            }
            "remove" => {
                self.items = self.items.iter().filter(|t| t.id != id).cloned().collect();
            }
            _ => return,
        }
        self.recount();
    }

    pub fn clear_done(&mut self) {
        self.items = self.items.iter().filter(|t| !t.done).cloned().collect();
        self.recount();
    }

    fn recount(&mut self) {
        self.left = self.items.iter().filter(|t| !t.done).count() as u32;
    }
}
