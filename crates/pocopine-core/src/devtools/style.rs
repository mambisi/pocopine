//! Devtools stylesheet + one-time injection.
//!
//! Lifted verbatim from the pre-split monolithic `devtools.rs`. The
//! selectors are namespaced `__pp_dev_*` / `__pp_jv_*` / `__pp_devtools_root`
//! so they don't collide with host-app CSS. Injected into `<head>` once
//! per session via [`inject`] from the devtools' `install` path.

use web_sys::window;

pub(crate) const STYLE_ID: &str = "__pp_devtools_style";

/// Attach the stylesheet to `<head>` if it isn't there already. No-op
/// when the element already exists (idempotent install).
pub(crate) fn inject() {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return;
    };
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let Ok(style) = doc.create_element("style") else {
        return;
    };
    let _ = style.set_attribute("id", STYLE_ID);
    style.set_text_content(Some(STYLESHEET));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    }
}

pub(crate) const STYLESHEET: &str = "\
    #__pp_devtools_root{position:fixed;top:12px;right:12px;width:540px;max-height:85vh;\
    overflow:hidden;z-index:2147483647;font-family:ui-monospace,Menlo,Consolas,monospace;\
    font-size:11px;line-height:1.45;color:#e6e1d8;background:#181715;\
    border:1px solid #2d2a24;border-radius:8px;box-shadow:0 10px 30px rgba(0,0,0,0.5);\
    display:flex;flex-direction:column;}\
    #__pp_devtools_root[data-collapsed=\"true\"]{max-height:none;}\
    #__pp_devtools_root[data-collapsed=\"true\"] .__pp_dev_meta,\
    #__pp_devtools_root[data-collapsed=\"true\"] .__pp_dev_body{display:none;}\
    #__pp_devtools_root *{box-sizing:border-box;}\
    #__pp_devtools_root,#__pp_devtools_root *{scrollbar-width:thin;\
    scrollbar-color:#3a3631 transparent;}\
    #__pp_devtools_root ::-webkit-scrollbar,\
    #__pp_devtools_root::-webkit-scrollbar{width:6px;height:6px;}\
    #__pp_devtools_root ::-webkit-scrollbar-track,\
    #__pp_devtools_root::-webkit-scrollbar-track{background:transparent;}\
    #__pp_devtools_root ::-webkit-scrollbar-thumb,\
    #__pp_devtools_root::-webkit-scrollbar-thumb{background:#3a3631;border-radius:3px;}\
    #__pp_devtools_root ::-webkit-scrollbar-thumb:hover,\
    #__pp_devtools_root::-webkit-scrollbar-thumb:hover{background:#ff6600;}\
    #__pp_devtools_root ::-webkit-scrollbar-corner,\
    #__pp_devtools_root::-webkit-scrollbar-corner{background:transparent;}\
    .__pp_dev_header{display:flex;align-items:center;justify-content:space-between;\
    padding:8px 12px;background:#252220;border-bottom:1px solid #2d2a24;\
    color:#ff6600;letter-spacing:0.06em;text-transform:uppercase;font-size:10px;flex:0 0 auto;}\
    #__pp_devtools_root[data-collapsed=\"true\"] .__pp_dev_header{border-bottom:none;}\
    .__pp_dev_actions{display:flex;align-items:center;gap:6px;}\
    .__pp_dev_btn{background:none;border:1px solid transparent;color:#e6e1d8;font:inherit;\
    cursor:pointer;padding:2px 6px;border-radius:3px;letter-spacing:0.05em;\
    text-transform:uppercase;font-size:9px;}\
    .__pp_dev_btn:hover{border-color:#ff6600;color:#ff6600;}\
    .__pp_dev_btn_on{background:#ff6600;color:#181715;}\
    .__pp_dev_btn_on:hover{border-color:#ff6600;color:#181715;}\
    .__pp_dev_close{font-size:14px;padding:0 6px;}\
    .__pp_dev_collapse{font-size:14px;padding:0 6px;}\
    .__pp_dev_seg{display:inline-flex;border:1px solid #2d2a24;border-radius:3px;overflow:hidden;}\
    .__pp_dev_seg_btn{background:none;border:none;color:#828282;font:inherit;\
    cursor:pointer;padding:2px 8px;letter-spacing:0.05em;text-transform:uppercase;font-size:9px;}\
    .__pp_dev_seg_btn:hover{color:#e6e1d8;}\
    .__pp_dev_seg_btn_on{background:#ff6600;color:#181715;}\
    .__pp_dev_seg_btn_on:hover{color:#181715;}\
    .__pp_dev_meta{padding:6px 12px;color:#828282;border-bottom:1px solid #2d2a24;flex:0 0 auto;}\
    .__pp_dev_body{display:flex;flex:1 1 auto;min-height:0;overflow:hidden;}\
    .__pp_dev_tree{flex:0 0 200px;overflow:auto;border-right:1px solid #2d2a24;padding:4px 0;}\
    .__pp_dev_tree_node{display:flex;justify-content:space-between;align-items:baseline;\
    padding:2px 10px 2px 6px;cursor:pointer;gap:6px;white-space:nowrap;}\
    .__pp_dev_tree_node:hover{background:#23211e;}\
    .__pp_dev_tree_node_on{background:#322a1d;}\
    .__pp_dev_tree_node_on:hover{background:#3a3021;}\
    .__pp_dev_detail{flex:1 1 auto;overflow:auto;padding:4px 8px 10px;}\
    .__pp_dev_detail_empty{padding:12px;}\
    .__pp_dev_scope{padding:6px 4px;}\
    .__pp_dev_scope_hd{display:flex;justify-content:space-between;align-items:baseline;\
    padding:2px 4px;}\
    .__pp_dev_tag{color:#ff6600;}\
    .__pp_dev_id{color:#828282;font-size:10px;}\
    .__pp_dev_kv{margin:6px 0 0;padding:0 4px;}\
    .__pp_dev_row{display:flex;gap:8px;padding:1px 0;}\
    .__pp_dev_row dt{color:#e6e1d8;flex:0 0 auto;min-width:7em;opacity:0.75;}\
    .__pp_dev_row dd{color:#c6e377;margin:0;word-break:break-all;cursor:pointer;}\
    .__pp_dev_row dd:hover{color:#fff;}\
    .__pp_dev_row dd.__pp_dev_copied{color:#181715;background:#ff6600;border-radius:2px;\
    padding:0 3px;}\
    .__pp_dev_empty{color:#828282;font-style:italic;padding:2px 4px;}\
    .__pp_dev_refs{margin-top:6px;color:#828282;font-size:10px;padding:2px 4px;}\
    .__pp_dev_refs code{color:#c6e377;}\
    .__pp_dev_json{margin-top:10px;border-top:1px dashed #2d2a24;padding-top:6px;}\
    .__pp_dev_json_label{color:#828282;font-size:9px;text-transform:uppercase;\
    letter-spacing:0.08em;margin-bottom:4px;padding:0 4px;}\
    .__pp_jv{padding:4px 6px;background:#131210;border:1px solid #2d2a24;border-radius:3px;\
    max-height:48vh;overflow:auto;}\
    .__pp_jv_group{margin:0;}\
    .__pp_jv_summary{cursor:pointer;list-style:none;padding:1px 0;outline:none;}\
    .__pp_jv_summary::-webkit-details-marker{display:none;}\
    .__pp_jv_summary::marker{content:\"\";}\
    .__pp_jv_summary::before{content:\"▸\";display:inline-block;width:1em;color:#828282;\
    font-size:9px;transform:translateY(-1px);}\
    .__pp_jv_group[open] > .__pp_jv_summary::before{content:\"▾\";}\
    .__pp_jv_body{padding-left:14px;border-left:1px dashed #2d2a24;margin-left:4px;}\
    .__pp_jv_row{padding:1px 0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}\
    .__pp_jv_key{color:#9ecbff;}\
    .__pp_jv_colon{color:#828282;margin:0 3px 0 1px;}\
    .__pp_jv_bracket{color:#e6e1d8;}\
    .__pp_jv_meta{color:#828282;margin:0 4px;font-style:italic;}\
    .__pp_jv_empty{color:#828282;font-style:italic;}\
    .__pp_jv_string{color:#c6e377;cursor:pointer;}\
    .__pp_jv_string:hover{color:#fff;}\
    .__pp_jv_number{color:#ff92c2;cursor:pointer;}\
    .__pp_jv_number:hover{color:#fff;}\
    .__pp_jv_bool{color:#ffb86c;cursor:pointer;}\
    .__pp_jv_bool:hover{color:#fff;}\
    .__pp_jv_null{color:#828282;cursor:pointer;}\
    .__pp_jv_null:hover{color:#fff;}\
    .__pp_jv_string.__pp_dev_copied,.__pp_jv_number.__pp_dev_copied,\
    .__pp_jv_bool.__pp_dev_copied,.__pp_jv_null.__pp_dev_copied{\
    background:#ff6600;color:#181715;border-radius:2px;padding:0 3px;}\
    .__pp_dev_highlight{outline:2px solid #ff6600 !important;}\
    \
    /* Tab strip (activated when ≥2 panels register). */\
    .__pp_dev_tabs{padding:4px 12px;background:#1f1d1b;border-bottom:1px solid #2d2a24;\
    flex:0 0 auto;}\
    .__pp_dev_tabs .__pp_dev_seg{width:auto;}\
    \
    /* Per-panel host divs sit inside the shell body. */\
    .__pp_dev_panel_host{flex:1 1 auto;min-height:0;display:flex;flex-direction:column;\
    overflow:hidden;}\
    \
    /* Scope inspector sub-header (mode toggle). */\
    .__pp_dev_scope_subhd{padding:6px 12px;border-bottom:1px solid #2d2a24;flex:0 0 auto;}\
    .__pp_dev_panes{display:flex;flex:1 1 auto;min-height:0;overflow:hidden;}\
    \
    /* Timeline panel. */\
    .__pp_dev_tl_head{display:grid;grid-template-columns:60px 42px 52px 1fr 60px;\
    gap:6px;padding:6px 12px;border-bottom:1px solid #2d2a24;color:#828282;\
    font-size:9px;text-transform:uppercase;letter-spacing:0.08em;flex:0 0 auto;}\
    .__pp_dev_tl_list{flex:1 1 auto;overflow:auto;padding:2px 0;}\
    .__pp_dev_tl_row{display:grid;grid-template-columns:60px 42px 52px 1fr 60px;\
    gap:6px;padding:2px 12px;align-items:baseline;white-space:nowrap;}\
    .__pp_dev_tl_row:hover{background:#23211e;}\
    .__pp_dev_tl_row_effect{color:#9ecbff;}\
    .__pp_dev_tl_row_handler{color:#c6e377;}\
    .__pp_dev_tl_col_t{color:#828282;font-variant-numeric:tabular-nums;}\
    .__pp_dev_tl_col_scope{color:#828282;font-size:10px;}\
    .__pp_dev_tl_col_dur{color:#828282;font-variant-numeric:tabular-nums;text-align:right;\
    font-size:10px;}\
    .__pp_dev_tl_chip{display:inline-block;width:20px;text-align:center;border-radius:2px;\
    font-size:9px;font-weight:700;letter-spacing:0.04em;padding:0 3px;}\
    .__pp_dev_tl_chip_effect{background:#1a2a40;color:#9ecbff;}\
    .__pp_dev_tl_chip_handler{background:#2a3a1c;color:#c6e377;}\
    .__pp_dev_tl_col_name{overflow:hidden;text-overflow:ellipsis;cursor:default;}\
    .__pp_dev_tl_col_name[data-copy]{cursor:pointer;}\
    .__pp_dev_tl_col_name[data-copy]:hover{color:#fff;}\
    .__pp_dev_tl_handler_args{color:#828282;margin-left:4px;}\
    \
    /* Flush queue + signal graph panel. */\
    .__pp_dev_gr_section{padding:6px 0;border-bottom:1px solid #2d2a24;}\
    .__pp_dev_gr_section:last-child{border-bottom:none;flex:1 1 auto;overflow:auto;}\
    .__pp_dev_gr_hd{padding:0 12px 4px;color:#828282;font-size:9px;\
    text-transform:uppercase;letter-spacing:0.08em;}\
    .__pp_dev_gr_queue{max-height:80px;overflow:auto;padding:0 12px;}\
    .__pp_dev_gr_row{display:flex;gap:8px;align-items:baseline;padding:1px 0;}\
    .__pp_dev_gr_id{color:#9ecbff;font-variant-numeric:tabular-nums;}\
    .__pp_dev_gr_chip{display:inline-block;border-radius:2px;font-size:9px;\
    font-weight:700;letter-spacing:0.04em;padding:0 6px;}\
    .__pp_dev_gr_chip_q{background:#1a2a40;color:#9ecbff;}\
    .__pp_dev_gr_chip_sched{background:#2a1c3a;color:#c79eff;}\
    .__pp_dev_gr_signals{padding:0 12px;}\
    .__pp_dev_gr_sig_head{display:grid;grid-template-columns:56px 52px 1fr;gap:6px;\
    padding:2px 0;color:#828282;font-size:9px;text-transform:uppercase;\
    letter-spacing:0.08em;border-bottom:1px dashed #2d2a24;}\
    .__pp_dev_gr_sig_row{display:grid;grid-template-columns:56px 52px 1fr;gap:6px;\
    padding:1px 0;align-items:baseline;}\
    .__pp_dev_gr_sig_row:hover{background:#23211e;}\
    .__pp_dev_gr_subs{color:#ffb86c;font-variant-numeric:tabular-nums;}\
    .__pp_dev_gr_lc{color:#828282;font-variant-numeric:tabular-nums;}\
    \
    /* Memory health panel. */\
    .__pp_dev_hl_alert{background:#3a1d1d;color:#ff9e9e;border-left:3px solid #ff3030;\
    padding:6px 10px;margin:6px 10px;font-size:10px;}\
    .__pp_dev_hl_cards{display:grid;grid-template-columns:1fr 1fr;gap:6px;\
    padding:8px 10px;overflow:auto;flex:1 1 auto;}\
    .__pp_dev_hl_card{background:#1f1d1b;border:1px solid #2d2a24;border-radius:4px;\
    padding:8px 10px;}\
    .__pp_dev_hl_card_leak{border-color:#ff3030;}\
    .__pp_dev_hl_hd{display:flex;justify-content:space-between;align-items:baseline;\
    color:#828282;font-size:9px;text-transform:uppercase;letter-spacing:0.08em;}\
    .__pp_dev_hl_peak{color:#828282;}\
    .__pp_dev_hl_value{font-size:18px;font-weight:700;margin:2px 0 4px;\
    font-variant-numeric:tabular-nums;}\
    .__pp_dev_hl_spark{width:100%;height:28px;}\
    .__pp_dev_hl_spark svg{width:100%;height:100%;display:block;}\
    \
    /* Router + inject panel. */\
    .__pp_dev_rt_section{padding:6px 0;border-bottom:1px solid #2d2a24;\
    display:flex;flex-direction:column;}\
    .__pp_dev_rt_section:last-child{border-bottom:none;flex:1 1 auto;overflow:auto;}\
    .__pp_dev_rt_hd{padding:0 12px 4px;color:#828282;font-size:9px;\
    text-transform:uppercase;letter-spacing:0.08em;}\
    .__pp_dev_rt_routes{max-height:140px;overflow:auto;padding:0 12px;}\
    .__pp_dev_rt_row{display:flex;gap:8px;align-items:baseline;padding:2px 0;\
    white-space:nowrap;}\
    .__pp_dev_rt_age{color:#828282;font-variant-numeric:tabular-nums;flex:0 0 auto;}\
    .__pp_dev_rt_path{color:#9ecbff;cursor:pointer;overflow:hidden;\
    text-overflow:ellipsis;flex:0 1 auto;}\
    .__pp_dev_rt_path:hover{color:#fff;}\
    .__pp_dev_rt_params{color:#ffb86c;font-size:10px;overflow:hidden;\
    text-overflow:ellipsis;}\
    .__pp_dev_rt_chain{padding:0 12px;}\
    .__pp_dev_rt_chain_hd{display:grid;grid-template-columns:1fr 1fr;gap:8px;\
    padding:2px 0;color:#828282;font-size:9px;text-transform:uppercase;\
    letter-spacing:0.08em;border-bottom:1px dashed #2d2a24;}\
    .__pp_dev_rt_chain_row{display:grid;grid-template-columns:1fr 1fr;gap:8px;\
    padding:1px 0;font-variant-numeric:tabular-nums;}\
    .__pp_dev_rt_chain_row:hover{background:#23211e;}\
    .__pp_dev_rt_key{color:#c79eff;}\
    .__pp_dev_rt_origin{color:#c6e377;}\
";
