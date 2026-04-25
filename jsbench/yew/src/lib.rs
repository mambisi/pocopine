use wasm_bindgen::prelude::*;
use web_sys::window;
use yew::prelude::*;

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

#[function_component(App)]
fn app() -> Html {
    let rows = use_state(Vec::<Row>::new);
    let selected_id = use_state(|| None::<usize>);
    let next_id = use_state(|| 1usize);
    let seed = use_state(|| 1u64);
    let last_action = use_state(|| "boot".to_string());
    let last_duration_ms = use_state(|| "0.00".to_string());
    let selected_label = use_state(|| "none".to_string());

    let run = {
        let rows = rows.clone();
        let selected_id = selected_id.clone();
        let selected_label = selected_label.clone();
        let next_id = next_id.clone();
        let seed = seed.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "run(1000)", || {
                let mut next = *next_id;
                let mut rng = *seed;
                let built = make_rows(1_000, &mut next, &mut rng);
                rows.set(built);
                selected_id.set(None);
                selected_label.set("none".to_string());
                next_id.set(next);
                seed.set(rng);
            });
        })
    };

    let run_lots = {
        let rows = rows.clone();
        let selected_id = selected_id.clone();
        let selected_label = selected_label.clone();
        let next_id = next_id.clone();
        let seed = seed.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "runLots(10000)", || {
                let mut next = *next_id;
                let mut rng = *seed;
                let built = make_rows(10_000, &mut next, &mut rng);
                rows.set(built);
                selected_id.set(None);
                selected_label.set("none".to_string());
                next_id.set(next);
                seed.set(rng);
            });
        })
    };

    let add = {
        let rows = rows.clone();
        let next_id = next_id.clone();
        let seed = seed.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "add(1000)", || {
                let mut next = *next_id;
                let mut rng = *seed;
                let more = make_rows(1_000, &mut next, &mut rng);
                let mut new_rows = (*rows).clone();
                new_rows.extend(more);
                rows.set(new_rows);
                next_id.set(next);
                seed.set(rng);
            });
        })
    };

    let update = {
        let rows = rows.clone();
        let selected_id = selected_id.clone();
        let selected_label = selected_label.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "update every 10th", || {
                let mut new_rows = (*rows).clone();
                for idx in (0..new_rows.len()).step_by(10) {
                    new_rows[idx].label.push_str(" !!!");
                }
                let next_label = match *selected_id {
                    Some(id) => new_rows
                        .iter()
                        .find(|row| row.id == id)
                        .map(|row| row.label.clone())
                        .unwrap_or_else(|| "none".to_string()),
                    None => "none".to_string(),
                };
                rows.set(new_rows);
                selected_label.set(next_label);
            });
        })
    };

    let clear = {
        let rows = rows.clone();
        let selected_id = selected_id.clone();
        let selected_label = selected_label.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "clear", || {
                rows.set(Vec::new());
                selected_id.set(None);
                selected_label.set("none".to_string());
            });
        })
    };

    let swap_rows = {
        let rows = rows.clone();
        let last_action = last_action.clone();
        let last_duration_ms = last_duration_ms.clone();
        Callback::from(move |_| {
            measure(&last_action, &last_duration_ms, "swapRows", || {
                let mut new_rows = (*rows).clone();
                if new_rows.len() > 998 {
                    new_rows.swap(1, 998);
                    rows.set(new_rows);
                }
            });
        })
    };

    let render_rows = (*rows)
        .iter()
        .cloned()
        .map(|row| {
            let row_id = row.id;
            let selected = *selected_id == Some(row_id);

            let on_select = {
                let rows = rows.clone();
                let selected_id = selected_id.clone();
                let selected_label = selected_label.clone();
                let last_action = last_action.clone();
                let last_duration_ms = last_duration_ms.clone();
                Callback::from(move |_| {
                    measure(&last_action, &last_duration_ms, "select", || {
                        selected_id.set(Some(row_id));
                        let label = (*rows)
                            .iter()
                            .find(|row| row.id == row_id)
                            .map(|row| row.label.clone())
                            .unwrap_or_else(|| "none".to_string());
                        selected_label.set(label);
                    });
                })
            };

            let on_remove = {
                let rows = rows.clone();
                let selected_id = selected_id.clone();
                let selected_label = selected_label.clone();
                let last_action = last_action.clone();
                let last_duration_ms = last_duration_ms.clone();
                Callback::from(move |_| {
                    measure(&last_action, &last_duration_ms, "remove", || {
                        let mut new_rows = (*rows).clone();
                        if let Some(idx) = new_rows.iter().position(|row| row.id == row_id) {
                            new_rows.remove(idx);
                        }
                        let next_selected = if *selected_id == Some(row_id) {
                            None
                        } else {
                            *selected_id
                        };
                        let next_label = match next_selected {
                            Some(id) => new_rows
                                .iter()
                                .find(|row| row.id == id)
                                .map(|row| row.label.clone())
                                .unwrap_or_else(|| "none".to_string()),
                            None => "none".to_string(),
                        };
                        rows.set(new_rows);
                        selected_id.set(next_selected);
                        selected_label.set(next_label);
                    });
                })
            };

            html! {
                <tr key={row.id} class={if selected { "danger" } else { "" }}>
                    <td class="col-md-1">{row.id}</td>
                    <td class="col-md-4">
                        <a onclick={on_select}>{row.label}</a>
                    </td>
                    <td class="col-md-1">
                        <a onclick={on_remove}>
                            <span class="glyphicon glyphicon-remove remove" aria-hidden="true"></span>
                        </a>
                    </td>
                    <td class="col-md-6"></td>
                </tr>
            }
        })
        .collect::<Html>();

    html! {
        <div class="container">
            <div class="jumbotron">
                <div class="row">
                    <div class="col-md-6">
                        <h1>{"yew keyed"}</h1>
                    </div>
                    <div class="col-md-6">
                        <div class="row">
                            <div class="col-sm-6 smallpad"><button id="run" type="button" onclick={run}>{"Create 1,000 rows"}</button></div>
                            <div class="col-sm-6 smallpad"><button id="runlots" type="button" onclick={run_lots}>{"Create 10,000 rows"}</button></div>
                            <div class="col-sm-6 smallpad"><button id="add" type="button" onclick={add}>{"Append 1,000 rows"}</button></div>
                            <div class="col-sm-6 smallpad"><button id="update" type="button" onclick={update}>{"Update every 10th row"}</button></div>
                            <div class="col-sm-6 smallpad"><button id="clear" type="button" onclick={clear}>{"Clear"}</button></div>
                            <div class="col-sm-6 smallpad"><button id="swaprows" type="button" onclick={swap_rows}>{"Swap Rows"}</button></div>
                        </div>
                    </div>
                </div>
                <div class="metrics">
                    <span>{"Last action: "}<strong>{(*last_action).clone()}</strong></span>
                    <span>{"Last duration: "}<strong>{(*last_duration_ms).clone()}</strong>{" ms"}</span>
                    <span>{"Rows: "}<strong>{rows.len()}</strong></span>
                    <span>{"Selected: "}<strong>{(*selected_label).clone()}</strong></span>
                </div>
            </div>

            <table class="table table-hover table-striped test-data">
                <tbody>{render_rows}</tbody>
            </table>

            <span class="preloadicon" aria-hidden="true">{"×"}</span>
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
    last_action: &UseStateHandle<String>,
    last_duration_ms: &UseStateHandle<String>,
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
        .expect("missing #app");
    yew::Renderer::<App>::with_root(root).render();
}
