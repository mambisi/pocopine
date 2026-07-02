//! RFC-112 — compile-pass: `#[derive(PathAccess)]` chains through
//! nested structs and Vecs; the `#[component]` dispatch arms
//! resolve via autoref for deriving AND non-deriving field types;
//! nested template paths coexist with RFC-111 validation.
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default, PathAccess)]
struct Limits {
    max: i32,
}

#[derive(Serialize, Deserialize, Clone, Default, PathAccess)]
struct Settings {
    theme: String,
    limits: Limits,
    items: Vec<Limits>,
    tags: Vec<String>,
}

/// No PathAccess — the component's dispatch arm must degrade to
/// the fallback through autoref, not fail to compile.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Plain {
    x: i32,
}

/// RFC-112 — `#[derive(Props)]` groups get a leaves-only
/// `PathAccess` impl for free: used as a NORMAL nested field,
/// `config.tab_name` descends natively.
#[derive(Serialize, Deserialize, Clone, Default, Props)]
struct TabProps {
    #[prop]
    tab_name: String,
    #[prop]
    tab_size: i32,
}

/// Explicit-list flatten container with a STRUCT leaf — the
/// leaf-rooted nested path (`panel_limits.max`) re-roots into the
/// container, whose own PathAccess derive descends natively.
#[derive(Serialize, Deserialize, Clone, Default, PathAccess)]
struct Panel {
    panel_limits: Limits,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pa-demo",
    template_inline = r#"<div>
  <input pp-model="settings.theme" />
  <span pp-text="settings.limits.max"></span>
  <span pp-text="plain.x"></span>
  <span pp-text="config.tab_name"></span>
  <input pp-model="panel_limits.max" />
  <button pp-on:click="grow">+</button>
</div>"#
)]
struct PaDemo {
    settings: Settings,
    plain: Plain,
    config: TabProps,
    #[prop(flatten = ["panel_limits"])]
    panel: Panel,
    count: i32,
}

#[handlers]
impl PaDemo {
    pub fn grow(&mut self) {
        self.settings.limits.max += 1;
    }
}

fn main() {}
