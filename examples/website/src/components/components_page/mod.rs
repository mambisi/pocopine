//! `/components/:name` — a Shadcn-style reference page for one Pine
//! primitive: a Preview/Code tab view (the live demo + its real
//! source), installation, anatomy, and a props table. The router
//! repaints this on every URL change, so `on_mount` re-derives the
//! page content from the `:name` slug.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::component_meta::{self, CATEGORY_ORDER, COMPONENTS};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PropView {
    pub key: String,
    pub element: String,
    pub name: String,
    pub ty: String,
    pub desc: String,
}

/// One row of the left-hand component nav — a category header or a
/// component link. Uses the shared `.side-nav-link` styling (matching
/// the docs sidebar).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NavRow {
    pub key: String,
    pub label: String,
    pub href: String,
    pub is_header: bool,
    pub active: bool,
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
    /// Flat left-nav rows (category headers + component links), with the
    /// current component marked active. Flattened because pocopine's
    /// nested `pp-for` doesn't iterate.
    pub nav: Vec<NavRow>,
}

fn build_nav(active_slug: &str) -> Vec<NavRow> {
    let mut rows = Vec::new();
    for &cat in CATEGORY_ORDER {
        rows.push(NavRow {
            key: format!("h:{cat}"),
            label: cat.into(),
            href: String::new(),
            is_header: true,
            active: false,
        });
        for c in COMPONENTS.iter().filter(|c| c.category == cat) {
            rows.push(NavRow {
                key: c.slug.into(),
                label: c.title.into(),
                href: format!("/components/{}", c.slug),
                is_header: false,
                active: c.slug == active_slug,
            });
        }
    }
    rows
}

#[handlers]
impl ComponentPage {
    // Build in on_setup (before first render) so the props `pp-for`
    // has data when the compiled template installs clone directives.
    pub fn on_setup(&mut self) {
        self.tab = "preview".into();
        self.nav = build_nav(&self.name);
        // Demo `.poco` source, syntect-highlighted at build time.
        self.code = crate::gen_code::component::code(&self.name).into();
        // Per-component install snippet + anatomy, syntect-highlighted
        // at build time (see build.rs / gen_code::api).
        self.install = crate::gen_code::api::install_for(&self.name).into();
        self.anatomy = crate::gen_code::api::anatomy_for(&self.name).into();
        self.has_anatomy = !self.anatomy.is_empty();

        if let Some(m) = component_meta::find(&self.name) {
            self.title = m.title.into();
            self.category = m.category.into();
            self.blurb = m.blurb.into();
            self.props = self.api_props(m);
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

    /// Delegated SPA nav for the sidebar links (pp-route can't live in a
    /// pp-for clone, so the `<a href="/components/…">` clicks bubble here).
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

impl ComponentPage {
    /// Build the props table — auto-extracted from the Pine source at
    /// build time (`gen_code::api`) when available, else the
    /// hand-written `component_meta` rows as a fallback. Not a handler.
    fn api_props(&self, m: &component_meta::ComponentMeta) -> Vec<PropView> {
        let auto = crate::gen_code::api::props_for(&self.name);
        if !auto.is_empty() {
            return auto
                .iter()
                .map(|p| PropView {
                    key: p.key.into(),
                    element: p.element.into(),
                    name: p.name.into(),
                    ty: p.ty.into(),
                    desc: p.desc.into(),
                })
                .collect();
        }
        m.props
            .iter()
            .map(|p| {
                let desc = if !p.default.is_empty() && p.default != "—" {
                    format!("Default: {}. {}", p.default, p.desc)
                } else {
                    p.desc.to_string()
                };
                PropView {
                    key: format!("meta.{}", p.name),
                    element: String::new(),
                    name: p.name.into(),
                    ty: p.ty.into(),
                    desc,
                }
            })
            .collect()
    }
}

impl RouteComponent for ComponentPage {}
