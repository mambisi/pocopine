//! RFC-099 SSR demo — server-render a real `#[component]` to HTML,
//! host-side, no browser. Run with:
//!
//! ```sh
//! cargo run -p ssr-demo
//! ```
//!
//! The printed `body` is what the server sends as first paint (zero
//! JS/wasm needed to display it); the `state island` is the JSON the
//! wasm client deserializes to *claim* that DOM during hydration. The
//! end-to-end render→hydrate round-trip is verified in
//! `crates/pocopine/tests/ssr_hydration.rs` (firefox).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "product-card",
    template_inline = r#"<div class="card">
    <h2 class="name" pp-text="name"></h2>
    <p class="price">{{currency}}{{price}}</p>
    <p class="stock" pp-show="in_stock">In stock</p>
    <button :data-id="id" :disabled="sold_out">Add {{name}} to cart</button>
</div>"#
)]
struct ProductCard {
    name: String,
    price: f64,
    currency: String,
    id: f64,
    in_stock: bool,
    sold_out: bool,
}

#[handlers]
impl ProductCard {}

fn main() {
    ProductCard::register();

    let card = ProductCard {
        name: "Pocopine Mug".into(),
        price: 12.5,
        currency: "$".into(),
        id: 42.0,
        in_stock: true,
        sold_out: false,
    };

    let page = pocopine_ssr::render_to_string(&card).expect("component is registered");

    println!("──────── server-rendered HTML (first paint, no wasm) ────────");
    println!("{}", page.body);
    println!("\n──────── state island (client deserializes to hydrate) ──────");
    println!("{}", page.state_island());
    println!("\n──────── full hydratable fragment ───────────────────────────");
    println!("{}", page.into_fragment());
}
