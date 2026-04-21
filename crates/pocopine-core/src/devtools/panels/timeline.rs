//! Timeline panel — unified feed of effect runs + handler invokes.
//!
//! Consumes the ring buffer `super::super::ring` populates via the
//! hooks `devtools::install` wires up. No extra state of its own;
//! the panel is a pure function of the ring's contents + the shell's
//! render tick.
//!
//! Fingerprint is `(len, last_seq)` — O(1). The render loop skips
//! the heavy `set_inner_html` when no new event has arrived.

use web_sys::Element;

use super::super::panel::Panel;
use super::super::ring::{self, TimelineEvent};
use super::super::util;

const LIST_ID: &str = "__pp_dev_tl_list";
const EMPTY_TEXT: &str =
    "No events yet. Interact with the page (click a handler, mutate a signal) to populate.";

pub(crate) struct Timeline;

impl Panel for Timeline {
    fn id(&self) -> &'static str {
        "timeline"
    }

    fn label(&self) -> &'static str {
        "Timeline"
    }

    fn mount(&self, host: &Element) {
        // Static shell: a header row with column labels + an empty
        // list host that `render` populates. No per-row state, so no
        // delegated handlers wire up here — the shell's root-level
        // click delegate covers `data-copy` cells.
        let html = format!(
            "<div class=\"__pp_dev_tl_head\">\
               <span class=\"__pp_dev_tl_col_t\">time</span>\
               <span class=\"__pp_dev_tl_col_kind\">kind</span>\
               <span class=\"__pp_dev_tl_col_scope\">scope</span>\
               <span class=\"__pp_dev_tl_col_name\">name</span>\
               <span class=\"__pp_dev_tl_col_dur\">dur</span>\
             </div>\
             <div id=\"{list}\" class=\"__pp_dev_tl_list\"></div>",
            list = LIST_ID,
        );
        host.set_inner_html(&html);
    }

    fn fingerprint(&self) -> String {
        format!("{}|{}", ring::len(), ring::last_seq())
    }

    fn render(&self, host: &Element) {
        let Some(list) = host.query_selector(&format!("#{LIST_ID}")).ok().flatten() else {
            return;
        };
        let events = ring::snapshot();
        if events.is_empty() {
            list.set_inner_html(&format!(
                r#"<div class="__pp_dev_empty">{}</div>"#,
                util::escape(EMPTY_TEXT)
            ));
            return;
        }

        // Newest at top.
        let now = ring::now_ms_for_scope();
        let mut html = String::new();
        for ev in events.iter().rev() {
            html.push_str(&render_row(ev, now));
        }
        list.set_inner_html(&html);
    }

    fn handle_action(&self, _action: &str, _el: &Element) -> bool {
        // Panel exposes no panel-scoped actions today. Copy-to-
        // clipboard still works on cells that carry `data-copy`
        // because the shell click delegate handles those on the
        // fall-through path.
        false
    }
}

fn render_row(ev: &TimelineEvent, now_ms: f64) -> String {
    match ev {
        TimelineEvent::EffectRun {
            id,
            scope,
            dur_us,
            t_ms,
            ..
        } => {
            let age = format_age(now_ms - *t_ms);
            let scope_text = scope
                .map(|s| format!("#{}", s.0))
                .unwrap_or_else(|| "—".into());
            format!(
                "<div class=\"__pp_dev_tl_row __pp_dev_tl_row_effect\">\
                   <span class=\"__pp_dev_tl_col_t\">{age}</span>\
                   <span class=\"__pp_dev_tl_col_kind\">\
                     <span class=\"__pp_dev_tl_chip __pp_dev_tl_chip_effect\">E</span>\
                   </span>\
                   <span class=\"__pp_dev_tl_col_scope\">{scope_text}</span>\
                   <span class=\"__pp_dev_tl_col_name\">effect #{eid}</span>\
                   <span class=\"__pp_dev_tl_col_dur\">{dur}</span>\
                 </div>",
                eid = id.0,
                dur = format_dur(*dur_us),
            )
        }
        TimelineEvent::Handler {
            scope,
            name,
            args_summary,
            dur_us,
            t_ms,
            ..
        } => {
            let age = format_age(now_ms - *t_ms);
            // Clip args to a single line and show full via title.
            let args_summary = util::escape(args_summary);
            let name_esc = util::escape(name);
            let name_col = if args_summary.is_empty() {
                format!(
                    "<span class=\"__pp_dev_tl_col_name\">\
                       <span class=\"__pp_dev_tl_handler_name\">{name_esc}</span>\
                     </span>"
                )
            } else {
                format!(
                    "<span class=\"__pp_dev_tl_col_name\" \
                           data-copy=\"{name_esc}({args_summary})\" \
                           title=\"click to copy\">\
                       <span class=\"__pp_dev_tl_handler_name\">{name_esc}</span>\
                       <span class=\"__pp_dev_tl_handler_args\">({args_summary})</span>\
                     </span>"
                )
            };
            format!(
                "<div class=\"__pp_dev_tl_row __pp_dev_tl_row_handler\">\
                   <span class=\"__pp_dev_tl_col_t\">{age}</span>\
                   <span class=\"__pp_dev_tl_col_kind\">\
                     <span class=\"__pp_dev_tl_chip __pp_dev_tl_chip_handler\">H</span>\
                   </span>\
                   <span class=\"__pp_dev_tl_col_scope\">#{sid}</span>\
                   {name_col}\
                   <span class=\"__pp_dev_tl_col_dur\">{dur}</span>\
                 </div>",
                sid = scope.0,
                dur = format_dur(*dur_us),
            )
        }
    }
}

/// Humanised "time since" string. Under 1s shows ms; under 60s
/// shows seconds with one decimal; otherwise minutes + seconds.
fn format_age(delta_ms: f64) -> String {
    let ms = delta_ms.max(0.0);
    if ms < 1000.0 {
        format!("{:>5.0}ms", ms)
    } else if ms < 60_000.0 {
        format!("{:>5.1}s", ms / 1000.0)
    } else {
        let secs = ms / 1000.0;
        let mins = (secs / 60.0).floor();
        let rem = secs - mins * 60.0;
        format!("{:>2.0}m{:02.0}s", mins, rem)
    }
}

fn format_dur(us: u32) -> String {
    if us < 1000 {
        format!("{us}µs")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}
