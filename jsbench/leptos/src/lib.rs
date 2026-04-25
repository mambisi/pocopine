use leptos::{ev, mount::mount_to, prelude::*};
use wasm_bindgen::prelude::*;
use web_sys::window;

const ADJECTIVES: &[&str] = &[
    "pretty", "large", "big", "small", "tall", "short", "long", "handsome", "plain",
    "quaint", "clean", "elegant", "easy", "angry", "crazy", "helpful", "mushy", "odd",
    "unsightly", "adorable", "important", "inexpensive", "cheap", "expensive", "fancy",
];

const COLOURS: &[&str] = &[
    "red", "yellow", "blue", "green", "pink", "brown", "purple", "brown", "white",
    "black", "orange",
];

const NOUNS: &[&str] = &[
    "table", "chair", "house", "bbq", "desk", "car", "pony", "cookie", "sandwich",
    "burger", "pizza", "mouse", "keyboard",
];

#[derive(Clone, PartialEq)]
struct Row {
    id: usize,
    label: String,
}

#[component]
fn App() -> impl IntoView {
    let rows = RwSignal::new(Vec::<Row>::new());
    let selected_id = RwSignal::new(None::<usize>);
    let next_id = RwSignal::new(1usize);
    let seed = RwSignal::new(1u64);
    let last_action = RwSignal::new("boot".to_string());
    let last_duration_ms = RwSignal::new("0.00".to_string());
    let selected_label = RwSignal::new("none".to_string());

    let run = move |_| {
        measure(last_action, last_duration_ms, "run(1000)", || {
            let mut next = next_id.get_untracked();
            let mut rng = seed.get_untracked();
            rows.set(make_rows(1_000, &mut next, &mut rng));
            selected_id.set(None);
            selected_label.set("none".to_string());
            next_id.set(next);
            seed.set(rng);
        });
    };

    let run_lots = move |_| {
        measure(last_action, last_duration_ms, "runLots(10000)", || {
            let mut next = next_id.get_untracked();
            let mut rng = seed.get_untracked();
            rows.set(make_rows(10_000, &mut next, &mut rng));
            selected_id.set(None);
            selected_label.set("none".to_string());
            next_id.set(next);
            seed.set(rng);
        });
    };

    let add = move |_| {
        measure(last_action, last_duration_ms, "add(1000)", || {
            let mut next = next_id.get_untracked();
            let mut rng = seed.get_untracked();
            let more = make_rows(1_000, &mut next, &mut rng);
            rows.update(|rows| rows.extend(more));
            next_id.set(next);
            seed.set(rng);
        });
    };

    let update_every_tenth = move |_| {
        measure(last_action, last_duration_ms, "update every 10th", || {
            rows.update(|rows| {
                for idx in (0..rows.len()).step_by(10) {
                    rows[idx].label.push_str(" !!!");
                }
            });
            let next_label = match selected_id.get_untracked() {
                Some(id) => rows
                    .get_untracked()
                    .iter()
                    .find(|row| row.id == id)
                    .map(|row| row.label.clone())
                    .unwrap_or_else(|| "none".to_string()),
                None => "none".to_string(),
            };
            selected_label.set(next_label);
        });
    };

    let clear = move |_| {
        measure(last_action, last_duration_ms, "clear", || {
            rows.set(Vec::new());
            selected_id.set(None);
            selected_label.set("none".to_string());
        });
    };

    let swap_rows = move |_| {
        measure(last_action, last_duration_ms, "swapRows", || {
            rows.update(|rows| {
                if rows.len() > 998 {
                    rows.swap(1, 998);
                }
            });
        });
    };

    view! {
        <div class="container">
            <div class="jumbotron">
                <div class="row">
                    <div class="col-md-6">
                        <h1>"leptos keyed"</h1>
                    </div>
                    <div class="col-md-6">
                        <div class="row">
                            <div class="col-sm-6 smallpad"><button id="run" type="button" on:click=run>"Create 1,000 rows"</button></div>
                            <div class="col-sm-6 smallpad"><button id="runlots" type="button" on:click=run_lots>"Create 10,000 rows"</button></div>
                            <div class="col-sm-6 smallpad"><button id="add" type="button" on:click=add>"Append 1,000 rows"</button></div>
                            <div class="col-sm-6 smallpad"><button id="update" type="button" on:click=update_every_tenth>"Update every 10th row"</button></div>
                            <div class="col-sm-6 smallpad"><button id="clear" type="button" on:click=clear>"Clear"</button></div>
                            <div class="col-sm-6 smallpad"><button id="swaprows" type="button" on:click=swap_rows>"Swap Rows"</button></div>
                        </div>
                    </div>
                </div>
                <div class="metrics">
                    <span>"Last action: "<strong>{move || last_action.get()}</strong></span>
                    <span>"Last duration: "<strong>{move || last_duration_ms.get()}</strong>" ms"</span>
                    <span>"Rows: "<strong>{move || rows.get().len()}</strong></span>
                    <span>"Selected: "<strong>{move || selected_label.get()}</strong></span>
                </div>
            </div>

            <table class="table table-hover table-striped test-data">
                <tbody>
                    <For
                        each=move || rows.get()
                        key=|row| row.id
                        children=move |row: Row| {
                            let row_id = row.id;
                            let select_row = move |_ev: ev::MouseEvent| {
                                measure(last_action, last_duration_ms, "select", || {
                                    selected_id.set(Some(row_id));
                                    let label = rows
                                        .get_untracked()
                                        .iter()
                                        .find(|row| row.id == row_id)
                                        .map(|row| row.label.clone())
                                        .unwrap_or_else(|| "none".to_string());
                                    selected_label.set(label);
                                });
                            };
                            let remove_row = move |_ev: ev::MouseEvent| {
                                measure(last_action, last_duration_ms, "remove", || {
                                    rows.update(|rows| {
                                        if let Some(idx) = rows.iter().position(|row| row.id == row_id) {
                                            rows.remove(idx);
                                        }
                                    });
                                    if selected_id.get_untracked() == Some(row_id) {
                                        selected_id.set(None);
                                        selected_label.set("none".to_string());
                                    }
                                });
                            };
                            view! {
                                <tr class=move || if selected_id.get() == Some(row_id) { "danger" } else { "" }>
                                    <td class="col-md-1">{row.id}</td>
                                    <td class="col-md-4">
                                        <a on:click=select_row>{row.label.clone()}</a>
                                    </td>
                                    <td class="col-md-1">
                                        <a on:click=remove_row>
                                            <span class="glyphicon glyphicon-remove remove" aria-hidden="true"></span>
                                        </a>
                                    </td>
                                    <td class="col-md-6"></td>
                                </tr>
                            }
                        }
                    />
                </tbody>
            </table>

            <span class="preloadicon" aria-hidden="true">"×"</span>
        </div>
    }
}

fn make_rows(count: usize, next_id: &mut usize, seed: &mut u64) -> Vec<Row> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let id = *next_id;
        *next_id += 1;
        out.push(Row {
            id,
            label: next_label(seed),
        });
    }
    out
}

fn next_label(seed: &mut u64) -> String {
    let adjective = ADJECTIVES[next_rand(seed, ADJECTIVES.len())];
    let colour = COLOURS[next_rand(seed, COLOURS.len())];
    let noun = NOUNS[next_rand(seed, NOUNS.len())];
    format!("{adjective} {colour} {noun}")
}

fn next_rand(seed: &mut u64, max: usize) -> usize {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    ((*seed >> 16) as usize) % max
}

fn measure<F: FnOnce()>(
    last_action: RwSignal<String>,
    last_duration_ms: RwSignal<String>,
    action: &'static str,
    f: F,
) {
    let start = now_ms();
    f();
    let end = now_ms();
    last_action.set(action.to_string());
    last_duration_ms.set(format!("{:.2}", end - start));
}

fn now_ms() -> f64 {
    window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[wasm_bindgen(start)]
pub fn main() {
    let root = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("app"))
        .expect("missing #app")
        .unchecked_into::<web_sys::HtmlElement>();

    mount_to(root, App).forget();
}
