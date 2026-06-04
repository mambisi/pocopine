//! "The whole stack" feature showcase. A row of chip tabs selects a
//! capability; the left column explains it and the right column shows a
//! syntax-highlighted code example. Both columns update from the active
//! tab (resolved from a delegated click via each tab's `data-feat`).
//!
//! The snippets are highlighted by syntect at build time (see
//! `build.rs` / `crate::gen_code::showcase`) — this component just
//! injects the finished HTML, so the wasm bundle carries no highlighter.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::gen_code::showcase::FEATS;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StackShowcase.poco", style = "stack_showcase.css")]
pub struct StackShowcase {
    pub active: u32,
    pub file: String,
    pub lang: String,
    pub title: String,
    pub desc: String,
    pub doc: String,
    /// Pre-highlighted code HTML (injected via pp-html).
    pub code_html: String,
}

#[handlers]
impl StackShowcase {
    pub fn on_setup(&mut self) {
        self.apply();
    }

    fn apply(&mut self) {
        let f = &FEATS[self.active as usize];
        self.file = f.file.into();
        self.lang = f.lang.into();
        self.title = f.title.into();
        self.desc = f.desc.into();
        self.doc = f.doc.into();
        self.code_html = f.code_html.into();
    }

    pub fn on_tab(&mut self, ev: web_sys::MouseEvent) {
        use wasm_bindgen::JsCast;
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if let Ok(Some(btn)) = el.closest("[data-feat]") {
            if let Some(v) = btn.get_attribute("data-feat") {
                if let Ok(n) = v.parse::<u32>() {
                    if (n as usize) < FEATS.len() {
                        self.active = n;
                        self.apply();
                    }
                }
            }
        }
    }
}
