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

    // Write a complete, openable document so you can *see* SSR in a browser:
    // it displays correctly with NO wasm/JS loaded — the pixels are in the
    // payload. The inert `data-pp-state` island is what the client would
    // later deserialize to hydrate. Output path: first CLI arg, else a temp
    // file (so the example never litters the repo).
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        let mut p = std::env::temp_dir();
        p.push("pocopine-ssr-demo.html");
        p.to_string_lossy().into_owned()
    });
    let doc = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>pocopine SSR demo — no wasm</title>\n<style>{CSS}</style>\n</head>\n\
         <body>\n<p class=\"banner\">Server-rendered by pocopine. No wasm loaded — \
         this is first paint.</p>\n{frag}\n</body>\n</html>\n",
        CSS = CSS,
        frag = page.into_fragment()
    );
    std::fs::write(&out, doc).expect("write preview html");
    println!("\n──────── open in a browser (renders with zero wasm) ─────────");
    println!("file://{out}");
}

const CSS: &str = "\
body{font:16px/1.5 system-ui,sans-serif;background:#1a130c;color:#f5ecdf;\
display:flex;flex-direction:column;align-items:center;gap:1.5rem;padding:3rem}\
.banner{color:#f8ae45;font-size:.85rem;letter-spacing:.02em}\
.card{background:#241a10;border:1px solid #3a2c1a;border-radius:12px;padding:1.5rem 1.75rem;\
width:min(22rem,90vw);box-shadow:0 8px 24px rgba(0,0,0,.4)}\
.card .name{margin:0 0 .25rem;font-size:1.4rem}\
.card .price{margin:0 0 .5rem;color:#f8ae45;font-weight:600;font-size:1.25rem}\
.card .stock{margin:0 0 1rem;color:#7bd88f;font-size:.8rem}\
.card button{background:#f8ae45;color:#1a130c;border:0;border-radius:8px;\
padding:.6rem 1rem;font-weight:600;cursor:pointer}\
.card button:disabled{opacity:.5;cursor:not-allowed}";
