//! Flush queue + signal graph panel.
//!
//! Two stacked sections:
//!
//! 1. **Queue** — every effect id currently waiting for the next
//!    microtask flush, flagged "scheduler-routed" (computeds etc.)
//!    vs. plain. Answers "why hasn't my effect fired yet?" — if
//!    you see your effect id sitting here, it's about to fire on
//!    the next microtask; if it's scheduler-routed, a computed's
//!    lazy get() will drive it.
//! 2. **Signals** — every signal with ≥1 subscriber, with its
//!    subscriber count and when it last changed. Answers "is this
//!    signal actually reactive?" + "who's watching it?".
//!
//! Both read from public `reactive::queue_snapshot()` /
//! `reactive::signal_graph_snapshot()` + the devtools-local
//! `signal_last` side table. No state of its own.

use web_sys::Element;

use crate::reactive;

use super::super::panel::Panel;
use super::super::ring;
use super::super::signal_last;

const QUEUE_ID: &str = "__pp_dev_gr_queue";
const SIG_ID: &str = "__pp_dev_gr_signals";

pub(crate) struct Graph;

impl Panel for Graph {
    fn id(&self) -> &'static str {
        "graph"
    }

    fn label(&self) -> &'static str {
        "Flush"
    }

    fn mount(&self, host: &Element) {
        let html = format!(
            "<div class=\"__pp_dev_gr_section\">\
               <div class=\"__pp_dev_gr_hd\">flush queue</div>\
               <div id=\"{q}\" class=\"__pp_dev_gr_queue\"></div>\
             </div>\
             <div class=\"__pp_dev_gr_section\">\
               <div class=\"__pp_dev_gr_hd\">signals</div>\
               <div id=\"{s}\" class=\"__pp_dev_gr_signals\"></div>\
             </div>",
            q = QUEUE_ID,
            s = SIG_ID,
        );
        host.set_inner_html(&html);
    }

    fn fingerprint(&self) -> String {
        let queue = reactive::queue_snapshot();
        let signals = reactive::signal_graph_snapshot();
        // Most-recent signal change drives "something moved" even
        // when queue + signal counts are identical — a same-key
        // write doesn't grow either collection but updates the
        // last-changed timestamp.
        let max_lc = signals
            .iter()
            .map(|s| signal_last::get(s.id).unwrap_or(0.0))
            .fold(0.0_f64, f64::max);
        format!("{}|{}|{}", queue.len(), signals.len(), max_lc as u64)
    }

    fn render(&self, host: &Element) {
        if let Some(q) = host.query_selector(&format!("#{QUEUE_ID}")).ok().flatten() {
            q.set_inner_html(&build_queue_html());
        }
        if let Some(s) = host.query_selector(&format!("#{SIG_ID}")).ok().flatten() {
            s.set_inner_html(&build_signals_html());
        }
    }

    fn handle_action(&self, _action: &str, _el: &Element) -> bool {
        false
    }
}

fn build_queue_html() -> String {
    let mut ids = reactive::queue_snapshot();
    ids.sort_by_key(|id| id.0);
    if ids.is_empty() {
        return r#"<div class="__pp_dev_empty">queue is empty</div>"#.into();
    }
    let mut html = String::with_capacity(ids.len() * 80);
    for id in ids {
        let routed = reactive::is_scheduler_routed(id);
        let chip = if routed {
            "<span class=\"__pp_dev_gr_chip __pp_dev_gr_chip_sched\">sched</span>"
        } else {
            "<span class=\"__pp_dev_gr_chip __pp_dev_gr_chip_q\">queued</span>"
        };
        html.push_str(&format!(
            "<div class=\"__pp_dev_gr_row\">\
               <span class=\"__pp_dev_gr_id\">#{eid}</span>\
               {chip}\
             </div>",
            eid = id.0,
        ));
    }
    html
}

fn build_signals_html() -> String {
    let mut rows = reactive::signal_graph_snapshot();
    // Sort by subscribers desc, then id asc — busiest signals at
    // the top, stable within a tier.
    rows.sort_by(|a, b| {
        b.subscribers
            .cmp(&a.subscribers)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    if rows.is_empty() {
        return r#"<div class="__pp_dev_empty">no signals with subscribers</div>"#.into();
    }
    let now = ring::now_ms_for_scope();
    let mut html = String::with_capacity(rows.len() * 120);
    html.push_str(
        "<div class=\"__pp_dev_gr_sig_head\">\
           <span>id</span><span>subs</span><span>last changed</span>\
         </div>",
    );
    for row in rows {
        let lc = signal_last::get(row.id);
        let lc_text = match lc {
            Some(t) => format_age(now - t),
            None => "—".into(),
        };
        html.push_str(&format!(
            "<div class=\"__pp_dev_gr_sig_row\">\
               <span class=\"__pp_dev_gr_id\">${sid}</span>\
               <span class=\"__pp_dev_gr_subs\">{subs}</span>\
               <span class=\"__pp_dev_gr_lc\">{lc_text}</span>\
             </div>",
            sid = row.id.0,
            subs = row.subscribers,
        ));
    }
    html
}

fn format_age(delta_ms: f64) -> String {
    let ms = delta_ms.max(0.0);
    if ms < 1000.0 {
        format!("{:>5.0}ms ago", ms)
    } else if ms < 60_000.0 {
        format!("{:>5.1}s ago", ms / 1000.0)
    } else {
        format!("{:.0}m ago", ms / 60_000.0)
    }
}
