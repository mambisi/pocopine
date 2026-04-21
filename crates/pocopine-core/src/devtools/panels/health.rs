//! Memory health panel.
//!
//! Four metric cards — listeners / effects / scopes / deps — each
//! with its current value, peak (over the sample ring), and a tiny
//! SVG sparkline showing the last HISTORY samples. A banner at the
//! top of the panel flags any metric whose entire back-half of the
//! sample window sits strictly above its front-half (the
//! "leak-detector" heuristic from `super::super::health`).
//!
//! No state of its own — reads the sample ring the shell's render
//! loop is already populating via `health::sample_tick`.

use wasm_bindgen::JsCast;
use web_sys::Element;

use super::super::health::{self, Leaks, Sample};
use super::super::panel::Panel;
use super::super::util;

const ALERT_ID: &str = "__pp_dev_hl_alert";
const CARDS_ID: &str = "__pp_dev_hl_cards";

pub(crate) struct Health;

impl Panel for Health {
    fn id(&self) -> &'static str {
        "health"
    }

    fn label(&self) -> &'static str {
        "Health"
    }

    fn mount(&self, host: &Element) {
        let html = format!(
            "<div id=\"{alert}\" class=\"__pp_dev_hl_alert\" style=\"display:none\"></div>\
             <div id=\"{cards}\" class=\"__pp_dev_hl_cards\"></div>",
            alert = ALERT_ID,
            cards = CARDS_ID,
        );
        host.set_inner_html(&html);
    }

    fn fingerprint(&self) -> String {
        let samples = health::snapshot();
        let last = samples.last().copied().unwrap_or_default();
        // Fingerprint on the latest sample; ticks that produce an
        // identical snapshot (flat state) skip the render.
        format!(
            "{}|{}|{}|{}|{}",
            samples.len(),
            last.listeners,
            last.effects,
            last.scopes,
            last.deps
        )
    }

    fn render(&self, host: &Element) {
        let samples = health::snapshot();
        let leaks = health::monotonic_growth(&samples);

        if let Some(alert) = host.query_selector(&format!("#{ALERT_ID}")).ok().flatten() {
            if leaks.any() {
                alert.set_inner_html(&build_alert_html(&leaks));
                if let Ok(html_el) = alert.clone().dyn_into::<web_sys::HtmlElement>() {
                    let _ = html_el.style().remove_property("display");
                }
            } else if let Ok(html_el) = alert.clone().dyn_into::<web_sys::HtmlElement>() {
                let _ = html_el.style().set_property("display", "none");
            }
        }

        if let Some(cards) = host.query_selector(&format!("#{CARDS_ID}")).ok().flatten() {
            cards.set_inner_html(&build_cards_html(&samples, &leaks));
        }
    }

    fn handle_action(&self, _action: &str, _el: &Element) -> bool {
        false
    }
}

fn build_alert_html(leaks: &Leaks) -> String {
    let mut leaked: Vec<&str> = Vec::new();
    if leaks.listeners {
        leaked.push("listeners");
    }
    if leaks.effects {
        leaked.push("effects");
    }
    if leaks.scopes {
        leaked.push("scopes");
    }
    if leaks.deps {
        leaked.push("deps");
    }
    format!(
        "<strong>leak suspected</strong> — {} grew monotonically across the last {} samples.",
        util::escape(&leaked.join(", ")),
        health::HISTORY,
    )
}

fn build_cards_html(samples: &[Sample], leaks: &Leaks) -> String {
    let last = samples.last().copied().unwrap_or_default();
    let peak_listeners = samples.iter().map(|s| s.listeners).max().unwrap_or(0);
    let peak_effects = samples.iter().map(|s| s.effects).max().unwrap_or(0);
    let peak_scopes = samples.iter().map(|s| s.scopes).max().unwrap_or(0);
    let peak_deps = samples.iter().map(|s| s.deps).max().unwrap_or(0);

    let listeners_series: Vec<usize> = samples.iter().map(|s| s.listeners).collect();
    let effects_series: Vec<usize> = samples.iter().map(|s| s.effects).collect();
    let scopes_series: Vec<usize> = samples.iter().map(|s| s.scopes).collect();
    let deps_series: Vec<usize> = samples.iter().map(|s| s.deps).collect();

    let cards = [
        (
            "listeners",
            last.listeners,
            peak_listeners,
            &listeners_series,
            "#9ecbff",
            leaks.listeners,
        ),
        (
            "effects",
            last.effects,
            peak_effects,
            &effects_series,
            "#c6e377",
            leaks.effects,
        ),
        (
            "scopes",
            last.scopes,
            peak_scopes,
            &scopes_series,
            "#ffb86c",
            leaks.scopes,
        ),
        (
            "deps",
            last.deps,
            peak_deps,
            &deps_series,
            "#c79eff",
            leaks.deps,
        ),
    ];

    let mut html = String::with_capacity(1024);
    for (label, value, peak, series, color, is_leaky) in cards {
        let leak_cls = if is_leaky { " __pp_dev_hl_card_leak" } else { "" };
        let spark = health::sparkline_svg(series, 200, 28, color);
        html.push_str(&format!(
            "<div class=\"__pp_dev_hl_card{leak}\">\
               <div class=\"__pp_dev_hl_hd\">\
                 <span class=\"__pp_dev_hl_label\">{label}</span>\
                 <span class=\"__pp_dev_hl_peak\">peak {peak}</span>\
               </div>\
               <div class=\"__pp_dev_hl_value\" style=\"color:{color}\">{value}</div>\
               <div class=\"__pp_dev_hl_spark\">{spark}</div>\
             </div>",
            leak = leak_cls,
        ));
    }
    html
}
