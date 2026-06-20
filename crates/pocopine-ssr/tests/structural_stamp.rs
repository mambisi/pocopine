//! RFC-099 Phase 3 Unit C — host tests for server-side structural
//! expansion (`pp-if` / `pp-match` / `pp-for`). Defines real
//! `#[component]`s, registers them, and asserts `render_to_string`
//! expands the right branch / case / rows with the right anchors —
//! entirely host-side (no browser). Client-side claiming of these
//! anchors is gated separately by the firefox differential harness.

use pocopine::prelude::*;
use pocopine_ssr::render_to_string;
use serde::{Deserialize, Serialize};

// ─── pp-if chain ────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-cond",
    template_inline = r#"<div class="root">
  <template pp-if="count > 5"><p class="big">big</p></template>
  <template pp-else-if="count > 0"><p class="small">small</p></template>
  <template pp-else><p class="zero">zero</p></template>
</div>"#
)]
struct SsrCond {
    count: i32,
}
#[handlers]
impl SsrCond {}

#[test]
fn cond_chain_renders_the_active_branch_with_anchor() {
    SsrCond::register();

    let big = render_to_string(&SsrCond { count: 10 }).unwrap().body;
    assert!(big.contains(r#"<p class="big">big</p>"#), "head: {big}");
    assert!(big.contains("<!--pp:cond:0-->"), "anchor: {big}");
    assert!(
        !big.contains("small") && !big.contains("zero"),
        "only one branch: {big}"
    );

    let small = render_to_string(&SsrCond { count: 3 }).unwrap().body;
    assert!(
        small.contains(r#"<p class="small">small</p>"#),
        "else-if: {small}"
    );
    assert!(
        !small.contains(">big<") && !small.contains("zero"),
        "only one branch: {small}"
    );

    let zero = render_to_string(&SsrCond { count: 0 }).unwrap().body;
    assert!(zero.contains(r#"<p class="zero">zero</p>"#), "else: {zero}");
    assert!(
        !zero.contains(">big<") && !zero.contains("small"),
        "only one branch: {zero}"
    );
}

// ─── pp-match (externally-tagged enum) ──────────────────────────────

#[derive(Default, Serialize, Deserialize)]
enum Status {
    #[default]
    Idle,
    Loading,
    Ready(String),
    Err {
        code: i32,
    },
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-match",
    template_inline = r#"<div class="root">
  <template pp-match="status">
    <template pp-case="Idle | Loading"><p class="pending">pending</p></template>
    <template pp-case="Ready" pp-let="msg"><p class="ready">{{msg}}</p></template>
    <template pp-case="_"><p class="other">other</p></template>
  </template>
</div>"#
)]
struct SsrMatch {
    status: Status,
}
#[handlers]
impl SsrMatch {}

#[test]
fn match_renders_the_selected_case_with_anchor() {
    SsrMatch::register();

    let pending = render_to_string(&SsrMatch {
        status: Status::Loading,
    })
    .unwrap()
    .body;
    assert!(
        pending.contains(r#"<p class="pending">pending</p>"#),
        "tag-list arm: {pending}"
    );
    assert!(pending.contains("<!--pp:match:0-->"), "anchor: {pending}");

    // pp-let payload bound into the case body's `{{msg}}`.
    let ready = render_to_string(&SsrMatch {
        status: Status::Ready("hi".into()),
    })
    .unwrap()
    .body;
    assert!(
        ready.contains(r#"<p class="ready">hi</p>"#),
        "pp-let payload: {ready}"
    );

    // Err{..} → no named case → the `_` wildcard.
    let other = render_to_string(&SsrMatch {
        status: Status::Err { code: 7 },
    })
    .unwrap()
    .body;
    assert!(
        other.contains(r#"<p class="other">other</p>"#),
        "wildcard: {other}"
    );
}

// ─── pp-for (unkeyed → body data path) ──────────────────────────────

#[derive(Clone, Default, Serialize, Deserialize)]
struct Row {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-for",
    template_inline = r#"<ul class="root">
  <template pp-for="row in rows"><li class="row" pp-text="row.label"></li></template>
</ul>"#
)]
struct SsrFor {
    rows: Vec<Row>,
}
#[handlers]
impl SsrFor {}

#[test]
fn for_renders_one_clone_per_item_with_anchor() {
    SsrFor::register();

    let body = render_to_string(&SsrFor {
        rows: vec![
            Row {
                id: 1,
                label: "alpha".into(),
            },
            Row {
                id: 2,
                label: "beta".into(),
            },
        ],
    })
    .unwrap()
    .body;

    assert!(
        body.contains(r#"<li class="row">alpha</li>"#),
        "row 0: {body}"
    );
    assert!(
        body.contains(r#"<li class="row">beta</li>"#),
        "row 1: {body}"
    );
    assert!(body.contains("<!--pp:for:0-->"), "anchor: {body}");
    assert_eq!(
        body.matches(r#"class="row""#).count(),
        2,
        "exactly two rows: {body}"
    );

    // Empty list → no rows, just the anchor.
    let empty = render_to_string(&SsrFor { rows: vec![] }).unwrap().body;
    assert!(
        empty.contains("<!--pp:for:0-->"),
        "anchor present when empty: {empty}"
    );
    assert!(
        !empty.contains("class=\"row\""),
        "no rows when empty: {empty}"
    );
}

// ─── pp-for ($index loop-local) ─────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-for-index",
    template_inline = r#"<ul class="root">
  <template pp-for="item in items"><li pp-text="$index"></li></template>
</ul>"#
)]
struct SsrForIndex {
    items: Vec<String>,
}
#[handlers]
impl SsrForIndex {}

#[test]
fn for_resolves_index_loop_local() {
    SsrForIndex::register();
    let body = render_to_string(&SsrForIndex {
        items: vec!["a".into(), "b".into(), "c".into()],
    })
    .unwrap()
    .body;
    assert!(body.contains("<li>0</li>"), "index 0: {body}");
    assert!(body.contains("<li>1</li>"), "index 1: {body}");
    assert!(body.contains("<li>2</li>"), "index 2: {body}");
}

// ─── RFC-099 Phase 4 — recursive child-component SSR ────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-kid",
    template_inline = r#"<span class="kid" pp-text="caption"></span>"#
)]
struct SsrKid {
    #[prop]
    caption: String,
}
#[handlers]
impl SsrKid {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-host",
    template_inline = r#"<div class="host"><ssr-kid caption="hello"></ssr-kid></div>"#
)]
struct SsrHost {}
#[handlers]
impl SsrHost {}

#[test]
fn recursive_child_ssr_renders_child_into_host_with_island() {
    SsrKid::register();
    SsrHost::register();
    let body = render_to_string(&SsrHost::default()).unwrap().body;

    // The <ssr-kid> host tag is still present, now containing the
    // recursively-rendered child root (with its prop stamped) + the
    // child's own data-pp-state island.
    assert!(body.contains("<ssr-kid"), "host tag present: {body}");
    assert!(
        body.contains(r#"data-pp-scope-id="ssr-kid""#),
        "child root carries its scope id: {body}"
    );
    assert!(
        body.contains(">hello</span>"),
        "child prop rendered: {body}"
    );
    assert!(
        body.contains("data-pp-state") && body.contains(r#""caption":"hello""#),
        "child state island present: {body}"
    );
}

// ─── RFC-099 Phase 4 — route SSR (render route into <pp-outlet>) ────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-route-page",
    template_inline = r#"<section class="page" pp-text="name"></section>"#
)]
struct SsrRoutePage {
    #[prop]
    name: String,
}
#[handlers]
impl SsrRoutePage {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ssr-shell",
    template_inline = r#"<div class="shell"><header>H</header><pp-outlet></pp-outlet></div>"#
)]
struct SsrShell {}
#[handlers]
impl SsrShell {}

#[test]
fn route_ssr_renders_matched_route_into_outlet() {
    SsrRoutePage::register();
    SsrShell::register();
    pocopine_core::router::register_route("/p/:name", "ssr-route-page");

    let page = pocopine_ssr::render_app_to_string(&SsrShell::default(), "/p/widgets").unwrap();

    assert!(
        page.body.contains("<pp-outlet>"),
        "outlet present: {}",
        page.body
    );
    assert!(
        page.body.contains("<ssr-route-page name=\"widgets\">"),
        "route host with param attr in outlet: {}",
        page.body
    );
    assert!(
        page.body.contains(">widgets</section>"),
        "route template rendered with the :name param: {}",
        page.body
    );
    assert!(
        page.body.contains("data-pp-state") && page.body.contains(r#""name":"widgets""#),
        "route state island present: {}",
        page.body
    );
}
