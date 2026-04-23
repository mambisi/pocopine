//! Router + inject-chain panel.
//!
//! Two stacked sections:
//!
//!   1. **Router** — recent route changes (path + params) pulled
//!      from the `router_log` ring the devtools `on_route_change`
//!      handler populates. Newest at top.
//!   2. **Inject chain** — for the scope currently selected in
//!      the Scope Inspector tab, every `(key_id, provider_scope)`
//!      pair reachable via the context chain. Uses
//!      `context::inject_chain`.
//!
//! No state of its own. Fingerprint is `(log_len, selected_id)` —
//! updates on new routes or when the user picks a different scope
//! in the inspector.

use web_sys::Element;

use crate::context;

use super::super::panel::Panel;
use super::super::router_log;
use super::super::util;
use super::scope as scope_panel;

const ROUTES_ID: &str = "__pp_dev_rt_routes";
const CHAIN_ID: &str = "__pp_dev_rt_chain";

pub(crate) struct RouterPanel;

impl Panel for RouterPanel {
    fn id(&self) -> &'static str {
        "router"
    }

    fn label(&self) -> &'static str {
        "Router"
    }

    fn mount(&self, host: &Element) {
        let html = format!(
            "<div class=\"__pp_dev_rt_section\">\
               <div class=\"__pp_dev_rt_hd\">recent routes</div>\
               <div id=\"{r}\" class=\"__pp_dev_rt_routes\"></div>\
             </div>\
             <div class=\"__pp_dev_rt_section\">\
               <div class=\"__pp_dev_rt_hd\">inject chain</div>\
               <div id=\"{c}\" class=\"__pp_dev_rt_chain\"></div>\
             </div>",
            r = ROUTES_ID,
            c = CHAIN_ID,
        );
        host.set_inner_html(&html);
    }

    fn fingerprint(&self) -> String {
        let sel = scope_panel::current_selection().map(|s| s.0).unwrap_or(0);
        format!("{}|{sel}", router_log::len())
    }

    fn render(&self, host: &Element) {
        if let Some(el) = host.query_selector(&format!("#{ROUTES_ID}")).ok().flatten() {
            el.set_inner_html(&build_routes_html());
        }
        if let Some(el) = host.query_selector(&format!("#{CHAIN_ID}")).ok().flatten() {
            el.set_inner_html(&build_chain_html());
        }
    }

    fn handle_action(&self, _action: &str, _el: &Element) -> bool {
        false
    }
}

fn build_routes_html() -> String {
    let entries = router_log::snapshot();
    if entries.is_empty() {
        return r#"<div class="__pp_dev_empty">no route changes observed yet</div>"#.into();
    }
    let now = super::super::ring::now_ms_for_scope();
    let mut html = String::with_capacity(entries.len() * 120);
    for entry in entries.iter().rev() {
        let age = format_age(now - entry.t_ms);
        let params = if entry.params.is_empty() {
            String::new()
        } else {
            let mut kv: Vec<(String, String)> = entry
                .params
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            kv.sort_by(|a, b| a.0.cmp(&b.0));
            let joined = kv
                .iter()
                .map(|(k, v)| format!("{}={}", util::escape(k), util::escape(v)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("<span class=\"__pp_dev_rt_params\">{}</span>", joined)
        };
        html.push_str(&format!(
            "<div class=\"__pp_dev_rt_row\">\
               <span class=\"__pp_dev_rt_age\">{age}</span>\
               <span class=\"__pp_dev_rt_path\" data-copy=\"{path_raw}\" title=\"click to copy\">{path}</span>\
               {params}\
             </div>",
            path_raw = util::escape(&entry.path),
            path = util::escape(&entry.path),
        ));
    }
    html
}

fn build_chain_html() -> String {
    let Some(scope) = scope_panel::current_selection() else {
        return r#"<div class="__pp_dev_empty">select a scope in the Scopes tab to view its inject chain</div>"#.into();
    };
    let chain = context::inject_chain(scope);
    if chain.is_empty() {
        return format!(
            r#"<div class="__pp_dev_empty">scope #{id} inherits no provided keys</div>"#,
            id = scope.0,
        );
    }
    let mut html = String::with_capacity(chain.len() * 100);
    html.push_str(
        "<div class=\"__pp_dev_rt_chain_hd\">\
           <span>key id</span><span>provided by</span>\
         </div>",
    );
    for (key_id, provider) in chain {
        let origin_marker = if provider == scope { " (self)" } else { "" };
        html.push_str(&format!(
            "<div class=\"__pp_dev_rt_chain_row\">\
               <span class=\"__pp_dev_rt_key\">#{key_id}</span>\
               <span class=\"__pp_dev_rt_origin\">#{provider}{origin_marker}</span>\
             </div>",
            provider = provider.0,
        ));
    }
    html
}

fn format_age(delta_ms: f64) -> String {
    let ms = delta_ms.max(0.0);
    if ms < 1000.0 {
        format!("{:>5.0}ms", ms)
    } else if ms < 60_000.0 {
        format!("{:>5.1}s", ms / 1000.0)
    } else {
        format!("{:.0}m", ms / 60_000.0)
    }
}
