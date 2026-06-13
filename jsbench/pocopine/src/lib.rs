use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::window;

const ADJECTIVES: &[&str] = &[
    "pretty",
    "large",
    "big",
    "small",
    "tall",
    "short",
    "long",
    "handsome",
    "plain",
    "quaint",
    "clean",
    "elegant",
    "easy",
    "angry",
    "crazy",
    "helpful",
    "mushy",
    "odd",
    "unsightly",
    "adorable",
    "important",
    "inexpensive",
    "cheap",
    "expensive",
    "fancy",
];

const COLOURS: &[&str] = &[
    "red", "yellow", "blue", "green", "pink", "brown", "purple", "brown", "white", "black",
    "orange",
];

const NOUNS: &[&str] = &[
    "table", "chair", "house", "bbq", "desk", "car", "pony", "cookie", "sandwich", "burger",
    "pizza", "mouse", "keyboard",
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: usize,
    pub label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "JsBenchApp.poco")]
pub struct JsBenchApp {
    pub rows: Vec<Row>,
    pub selected_id: Option<usize>,
    pub next_id: usize,
    pub seed: u64,
    pub last_action: String,
    pub last_duration_ms: String,
    pub selected_label: String,
}

#[handlers]
impl JsBenchApp {
    pub fn on_mount(&mut self) {
        self.seed = 1;
        self.last_action = "boot".into();
        self.last_duration_ms = "0.00".into();
        self.selected_label = "none".into();
    }

    pub fn run(&mut self) {
        self.measure("run(1000)", |s| {
            s.rows = s.build_rows(1_000);
            s.selected_id = None;
            s.selected_label = "none".into();
        });
    }

    pub fn run_lots(&mut self) {
        self.measure("runLots(10000)", |s| {
            s.rows = s.build_rows(10_000);
            s.selected_id = None;
            s.selected_label = "none".into();
        });
    }

    pub fn add(&mut self) {
        self.measure("add(1000)", |s| {
            let start = s.rows.len();
            let more = s.make_rows(1_000);
            s.rows.extend(more);
            pocopine::append_list_inline("rows", start, &s.rows[start..]);
        });
    }

    pub fn prepend(&mut self) {
        self.measure("prepend(1000)", |s| {
            let mut old = std::mem::take(&mut s.rows);
            s.rows = s.make_rows(1_000);
            s.rows.append(&mut old);
            pocopine::prepend_list_inline("rows", &s.rows[..1_000]);
        });
    }

    pub fn reorder_all(&mut self) {
        self.measure("reorder all", |s| {
            s.rows.reverse();
            pocopine::replace_field_inline("rows", &s.rows);
        });
    }

    pub fn update_every_tenth(&mut self) {
        self.measure("update every 10th", |s| {
            for idx in (0..s.rows.len()).step_by(10) {
                s.rows[idx].label.push_str(" !!!");
            }
            let patches: Vec<(usize, &Row)> = (0..s.rows.len())
                .step_by(10)
                .map(|idx| (idx, &s.rows[idx]))
                .collect();
            pocopine::patch_list_indices_inline("rows", &patches);
            if let Some(selected_id) = s.selected_id {
                s.selected_label = s
                    .rows
                    .iter()
                    .find(|row| row.id == selected_id)
                    .map(|row| row.label.clone())
                    .unwrap_or_else(|| "none".into());
            }
        });
    }

    pub fn clear(&mut self) {
        self.measure("clear", |s| {
            s.rows.clear();
            s.selected_id = None;
            s.selected_label = "none".into();
        });
    }

    pub fn swap_rows(&mut self) {
        self.measure("swapRows", |s| {
            if s.rows.len() > 998 {
                s.rows.swap(1, 998);
                pocopine::swap_list_indices_inline("rows", 1, 998);
            }
        });
    }

    pub fn select_row(&mut self, id: usize) {
        self.measure("select", |s| {
            s.selected_id = Some(id);
            s.selected_label = s
                .rows
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.label.clone())
                .unwrap_or_else(|| "none".into());
        });
    }

    pub fn remove_row(&mut self, id: usize) {
        self.measure("remove", |s| {
            if let Some(idx) = s.rows.iter().position(|row| row.id == id) {
                s.rows.remove(idx);
                pocopine::remove_list_at_inline("rows", idx);
            }
            if s.selected_id == Some(id) {
                s.selected_id = None;
                s.selected_label = "none".into();
            }
        });
    }
}

impl JsBenchApp {
    fn build_rows(&mut self, count: usize) -> Vec<Row> {
        self.make_rows(count)
    }

    fn make_rows(&mut self, count: usize) -> Vec<Row> {
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            self.next_id += 1;
            rows.push(Row {
                id: self.next_id,
                label: self.next_label(),
            });
        }
        rows
    }

    fn next_label(&mut self) -> String {
        let adjective = ADJECTIVES[self.next_rand(ADJECTIVES.len())];
        let colour = COLOURS[self.next_rand(COLOURS.len())];
        let noun = NOUNS[self.next_rand(NOUNS.len())];
        format!("{adjective} {colour} {noun}")
    }

    fn next_rand(&mut self, max: usize) -> usize {
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        ((self.seed >> 16) as usize) % max
    }

    fn measure(&mut self, action: &str, f: impl FnOnce(&mut Self)) {
        if pocopine::mount_profile_enabled() {
            pocopine::reset_mount_profile();
        }
        let start = now_ms();
        f(self);
        let end = now_ms();
        self.last_action = action.into();
        self.last_duration_ms = format!("{:.2}", end - start);
        schedule_mount_profile_report(action);
    }
}

fn schedule_mount_profile_report(action: &str) {
    if !pocopine::mount_profile_enabled() {
        return;
    }
    let action = action.to_string();
    let cb = Closure::once(move || {
        pocopine::report_mount_profile(&action);
    });
    if let Some(w) = window() {
        let _ =
            w.set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 0);
    }
    cb.forget();
}

fn now_ms() -> f64 {
    window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<JsBenchApp>().run();
}
