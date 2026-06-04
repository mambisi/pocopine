//! `/components/:name` — a Shadcn-style reference page for one Pine
//! primitive: a Preview/Code tab view (the live demo + its real
//! source), installation, anatomy, and a props table. The router
//! repaints this on every URL change, so `on_mount` re-derives the
//! page content from the `:name` slug.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::component_meta;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PropView {
    pub name: String,
    pub ty: String,
    pub default: String,
    pub desc: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "ComponentPage.poco",
    style = "component_page.css",
    role = "panel"
)]
pub struct ComponentPage {
    /// Route param `:name` — the component slug.
    #[prop]
    pub name: String,
    /// Visible tab: `"preview"` | `"code"`.
    pub tab: String,
    pub title: String,
    pub category: String,
    pub blurb: String,
    pub code: String,
    pub install: String,
    pub anatomy: String,
    pub has_anatomy: bool,
    pub has_props: bool,
    pub not_found: bool,
    pub props: Vec<PropView>,
}

#[handlers]
impl ComponentPage {
    // Build in on_setup (before first render) so the props `pp-for`
    // has data when the compiled template installs clone directives.
    pub fn on_setup(&mut self) {
        self.tab = "preview".into();
        // Demo `.poco` source, syntect-highlighted at build time.
        self.code = crate::gen_code::component::code(&self.name).into();
        self.install =
            "cargo add pocopine pine\n\n// then, in fn main():\npine::register_all();".into();

        if let Some(m) = component_meta::find(&self.name) {
            self.title = m.title.into();
            self.category = m.category.into();
            self.blurb = m.blurb.into();
            self.anatomy = m.anatomy.into();
            self.has_anatomy = !m.anatomy.is_empty();
            self.props = m
                .props
                .iter()
                .map(|p| PropView {
                    name: p.name.into(),
                    ty: p.ty.into(),
                    default: p.default.into(),
                    desc: p.desc.into(),
                })
                .collect();
            self.has_props = !self.props.is_empty();
        } else {
            self.not_found = true;
            self.title = "Unknown component".into();
            self.category = "Components".into();
            self.blurb = String::new();
        }
    }

    pub fn show_preview(&mut self) {
        self.tab = "preview".into();
    }

    pub fn show_code(&mut self) {
        self.tab = "code".into();
    }
}

impl RouteComponent for ComponentPage {}
