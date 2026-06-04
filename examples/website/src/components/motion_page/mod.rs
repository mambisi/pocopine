//! `/motion/:name` — a per-technique reference page, modelled on the
//! component reference. The catalogue sidebar lists the techniques; each
//! page shows that technique's own live demo (one subsection of
//! `<animation-demo only="…">`), its code, and a focused API table. The
//! router remounts this on every URL change, so `on_setup` re-derives
//! content from `:name`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{delegate_nav, Row2, Row4};
use crate::gen_code::motion;

/// Catalogue of techniques. `slug` is the route param, the
/// `<animation-demo only="…">` key, and (via `code_key`) the
/// `motion::code` snippet key.
struct TechMeta {
    slug: &'static str,
    title: &'static str,
    code_key: &'static str,
    blurb: &'static str,
}

const TECHS: &[TechMeta] = &[
    TechMeta {
        slug: "springs",
        title: "Springs",
        code_key: "spring",
        blurb: "Physics springs in the react-motion / flip-toolkit vocabulary — click a box to feel gentle, stiff, and wobbly.",
    },
    TechMeta {
        slug: "easing",
        title: "Easing",
        code_key: "easing",
        blurb: "Named cubic-bezier presets — click a button to send the box across with that curve.",
    },
    TechMeta {
        slug: "stagger",
        title: "Stagger",
        code_key: "stagger",
        blurb: "A per-index delay with a selectable origin — cascade the grid from first, last, or center.",
    },
    TechMeta {
        slug: "state",
        title: "Animate to state",
        code_key: "state",
        blurb: "Drive transform and depth with sliders; the box springs to each new target value.",
    },
    TechMeta {
        slug: "hover",
        title: "Hover",
        code_key: "hover",
        blurb: "Pointer-enter springs a lift + shadow; leave springs it back. Hover the card.",
    },
    TechMeta {
        slug: "focus",
        title: "Focus",
        code_key: "focus",
        blurb: "Focus springs a ring in, blur springs it out — keyboard-friendly feedback.",
    },
    TechMeta {
        slug: "press",
        title: "Press",
        code_key: "press",
        blurb: "Pointer-down springs the button down; release bounces it back.",
    },
    TechMeta {
        slug: "pan",
        title: "Pan",
        code_key: "pan",
        blurb: "Raw pointer tracking — drag across the zone to read live delta and velocity.",
    },
    TechMeta {
        slug: "tilt",
        title: "3D tilt",
        code_key: "tilt",
        blurb: "Pointer position rotates the card in 3D, with child layers lifting on hover.",
    },
    TechMeta {
        slug: "drag",
        title: "Drag",
        code_key: "drag",
        blurb: "Drag the card; on release a spring continues the fling velocity to rest, clamped to its bounds.",
    },
    TechMeta {
        slug: "scroll",
        title: "Scroll progress",
        code_key: "scroll",
        blurb: "A bar driven by page-scroll progress (0–1). Scroll the page to watch it fill.",
    },
    TechMeta {
        slug: "inview",
        title: "In-view reveal",
        code_key: "inview",
        blurb: "Fires once when an element scrolls into view — fade + rise it in.",
    },
    TechMeta {
        slug: "flip",
        title: "Shared layout (FLIP)",
        code_key: "layout",
        blurb: "Snapshot positions, reorder the DOM, then play the delta — tiles spring to their new spots.",
    },
];

/// One sidebar catalogue row.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NavRow {
    pub slug: String,
    pub title: String,
    pub href: String,
    pub active: bool,
}

#[derive(Serialize, Deserialize)]
#[component(
    template = "MotionPage.poco",
    style = "motion_page.css",
    role = "panel"
)]
pub struct MotionPage {
    /// Route param `:name` — the technique slug (also the demo `only` key).
    #[prop]
    pub name: String,
    pub title: String,
    pub blurb: String,
    pub code: String,
    /// The slug resolved to a known technique.
    pub found: bool,
    /// Show the spring-presets table (springs page).
    pub show_springs: bool,
    pub not_found: bool,
    pub nav: Vec<NavRow>,
    pub spring_presets: Vec<Row4>,
    pub functions: Vec<Row2>,
}

impl Default for MotionPage {
    fn default() -> Self {
        Self {
            name: String::new(),
            title: String::new(),
            blurb: String::new(),
            code: String::new(),
            found: false,
            show_springs: false,
            not_found: false,
            nav: Vec::new(),
            spring_presets: spring_presets(),
            functions: Vec::new(),
        }
    }
}

#[handlers]
impl MotionPage {
    pub fn on_setup(&mut self) {
        // Bare `/motion` (or an unknown slug) defaults to the first one.
        if self.name.is_empty() {
            self.name = "springs".into();
        }
        self.nav = build_nav(&self.name);

        if let Some(m) = TECHS.iter().find(|t| t.slug == self.name) {
            self.found = true;
            self.title = m.title.into();
            self.blurb = m.blurb.into();
            self.code = motion::code(m.code_key).into();
            self.show_springs = self.name == "springs";
            self.functions = functions_for(&self.name);
        } else {
            self.not_found = true;
            self.title = "Unknown technique".into();
            self.blurb = String::new();
        }
    }

    /// Delegated sidebar nav (`<a href="/motion/…">` clicks bubble here).
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        delegate_nav(&ev);
    }
}

impl RouteComponent for MotionPage {}

fn build_nav(active: &str) -> Vec<NavRow> {
    TECHS
        .iter()
        .map(|t| NavRow {
            slug: t.slug.into(),
            title: t.title.into(),
            href: format!("/motion/{}", t.slug),
            active: t.slug == active,
        })
        .collect()
}

/// Spring presets → stiffness / damping / mass.
fn spring_presets() -> Vec<Row4> {
    vec![
        Row4::new("Spring::gentle()", "120", "14", "1"),
        Row4::new("Spring::very_gentle()", "130", "17", "1"),
        Row4::new("Spring::wobbly()", "180", "12", "1"),
        Row4::new("Spring::no_wobble()", "200", "26", "1"),
        Row4::new("Spring::stiff()", "260", "26", "1"),
    ]
}

/// The pine-motion functions relevant to a technique (Signature → desc).
fn functions_for(slug: &str) -> Vec<Row2> {
    let animate = Row2::new(
        "animate(el, channels, timing)",
        "Animate CSS channels with a Tween, Spring, or Easing — compiled to a GPU linear() animation.",
    );
    match slug {
        "springs" => vec![
            animate,
            Row2::new(
                "Spring::gentle() / stiff() / wobbly()",
                "Named physics-spring presets; pass one as the timing.",
            ),
        ],
        "easing" => vec![
            animate,
            Row2::new(
                "Tween::new().easing(Easing::…)",
                "EASE_OUT_QUAD / EXPO / BACK / CIRC / APPLE, or CubicBezier([..]).",
            ),
        ],
        "stagger" => vec![Row2::new(
            "stagger(each_ms).from(origin)",
            "Per-index delay; delay_for(i, total) gives each element's offset.",
        )],
        "state" => vec![
            animate,
            Row2::new(
                "Spring::wobbly()",
                "A springy preset that suits state transitions.",
            ),
        ],
        "hover" => vec![
            Row2::new(
                "raise(el) / scale(el, factor)",
                "Spring a lift + shadow (or a scale) on pointer hover.",
            ),
            Row2::new(
                "hover(el, |entered, _| …)",
                "Lower-level hover callback for fully custom animation.",
            ),
        ],
        "focus" => vec![Row2::new(
            "focus(el, |focused, _| …)",
            "Fires on focusin / focusout — animate a focus ring in and out.",
        )],
        "press" => vec![Row2::new(
            "press(el, |start| Option<end>)",
            "Fires on pointer-down; the end handler gets a tap-success flag.",
        )],
        "tilt" => vec![Row2::new(
            "tilt(el, TiltConfig)",
            "3D pointer tilt; children with data-pm-tilt lift on hover.",
        )],
        "drag" => vec![Row2::new(
            "drag(el, DragConfig)",
            "Pointer drag with optional momentum + release spring; returns a GestureHandle.",
        )],
        "pan" => vec![Row2::new(
            "pan(el, PanConfig, |ev| …)",
            "Threshold-gated pan with PanEvent delta + velocity in the callback.",
        )],
        "scroll" => vec![Row2::new(
            "scroll_progress(|t| …)",
            "Calls back with 0..=1 page scroll progress.",
        )],
        "inview" => vec![Row2::new(
            "on_view(el, ViewConfig, |shown| …)",
            "IntersectionObserver — fires when the element enters / leaves view.",
        )],
        "flip" => vec![
            Row2::new(
                "snapshot_layout(el)",
                "Capture layout-tagged (data-pp-layout-id) rects before a DOM change.",
            ),
            Row2::new(
                "play_layout(el, snap, spring)",
                "Animate each tracked node from its old rect to its new one.",
            ),
        ],
        _ => vec![animate],
    }
}
