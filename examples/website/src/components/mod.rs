//! Page components. Each subdirectory holds one `#[component]`
//! plus its sibling `.poco` template — the same layout the `hn`
//! example uses.

pub mod component_meta;
pub mod components_index;
pub mod components_page;
pub mod doc_page;
pub mod hero;
pub mod install_cmd;
pub mod landing;
pub mod learn_todo;
pub mod showcase;
pub mod showcase_card;
pub mod site_header;
pub mod stack_showcase;
pub mod todo_demo;
pub mod tutorial;

pub use components_index::ComponentsIndex;
pub use components_page::ComponentPage;
pub use doc_page::DocPage;
pub use hero::Hero;
pub use install_cmd::InstallCmd;
pub use landing::Landing;
pub use learn_todo::LearnTodo;
pub use showcase::Showcase;
pub use showcase_card::ShowcaseCard;
pub use site_header::SiteHeader;
pub use stack_showcase::StackShowcase;
pub use todo_demo::TodoDemo;
pub use tutorial::Tutorial;

/// Shared SPA-navigation click delegate.
///
/// `pp-route` cannot be used inside a `<template pp-for>` clone — it
/// makes the compiled clone body render as raw HTML (directives never
/// install). Instead, list components put `pp-on:click` on their root
/// and call this: it finds the clicked internal `<a href="/…">` and
/// routes it client-side, leaving external / `#` links alone.
pub fn delegate_nav(ev: &web_sys::MouseEvent) {
    use wasm_bindgen::JsCast;
    let Some(el) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let Ok(Some(anchor)) = el.closest("a[href]") else {
        return;
    };
    if let Some(href) = anchor.get_attribute("href") {
        if href.starts_with('/') {
            ev.prevent_default();
            pocopine::navigate(&href);
        }
    }
}
