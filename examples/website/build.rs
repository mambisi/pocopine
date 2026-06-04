//! Build-time documentation renderer.
//!
//! Reads `../../docs/site.toml` (the nav manifest) and renders every
//! listed markdown page to an HTML fragment with `pulldown-cmark`
//! (host-side — pulldown-cmark does not build for wasm). The result
//! is emitted as a generated Rust module in `OUT_DIR` and embedded in
//! the wasm bundle via `include!`, so the docs ship as static pages
//! with no runtime markdown parser and no per-page fetch.
//!
//! Generated API (see `crate::docs_data`):
//!   pub static NAV: &[NavItem]
//!   pub fn page_html(slug) -> &'static str
//!   pub fn page_title(slug) -> &'static str
//!   pub fn page_toc(slug)  -> &'static [TocItem]

use std::collections::HashSet;
use std::{env, fs, path::Path};

use pulldown_cmark::{html, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

#[derive(Deserialize)]
struct Site {
    #[serde(default)]
    group: Vec<Group>,
}
#[derive(Deserialize)]
struct Group {
    section: String,
    title: String,
    #[serde(default)]
    pages: Vec<Pg>,
}
#[derive(Deserialize)]
struct Pg {
    title: String,
    path: String,
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let docs_dir = Path::new(&manifest).join("../../docs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", docs_dir.display());

    let out_dir = env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("docs_data.rs");

    // Rendered HTML fragments are written to a served static dir — NOT
    // embedded in the wasm. Only the small NAV + TOC metadata go into
    // `docs_data.rs`, so the bundle stays lean and the docs ship as
    // static pages the client fetches per route.
    let static_docs = Path::new(&manifest).join("static-docs");
    let _ = fs::remove_dir_all(&static_docs);
    fs::create_dir_all(&static_docs).ok();

    let site: Site = fs::read_to_string(docs_dir.join("site.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or(Site { group: vec![] });

    let mut code = String::new();
    code.push_str(
        "pub struct NavItem { pub title: &'static str, pub slug: &'static str, \
         pub section: &'static str, pub group: &'static str }\n",
    );
    code.push_str(
        "pub struct TocItem { pub depth: u8, pub text: &'static str, pub id: &'static str }\n",
    );

    let mut nav = String::from("pub static NAV: &[NavItem] = &[\n");
    let mut title_arms = String::new();
    let mut toc_arms = String::new();
    let mut seen = HashSet::new();

    for g in &site.group {
        for p in &g.pages {
            let slug = p.path.strip_suffix(".md").unwrap_or(&p.path);
            nav.push_str(&format!(
                "  NavItem {{ title: {:?}, slug: {:?}, section: {:?}, group: {:?} }},\n",
                p.title, slug, g.section, g.title
            ));
            if !seen.insert(slug.to_string()) {
                continue;
            }
            let raw = fs::read_to_string(docs_dir.join(&p.path)).unwrap_or_default();
            let body = strip_frontmatter(&raw);
            let (page_html, toc) = render(&body, &p.path);
            let frag = static_docs.join(format!("{slug}.html"));
            if let Some(parent) = frag.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::write(&frag, &page_html).ok();
            title_arms.push_str(&format!("        {:?} => {:?},\n", slug, p.title));
            let mut toc_lit = String::from("&[");
            for (depth, text, id) in &toc {
                toc_lit.push_str(&format!(
                    "TocItem {{ depth: {}, text: {:?}, id: {:?} }}, ",
                    depth, text, id
                ));
            }
            toc_lit.push(']');
            toc_arms.push_str(&format!("        {:?} => {},\n", slug, toc_lit));
        }
    }
    nav.push_str("];\n");

    code.push_str(&nav);
    code.push_str(&format!(
        "pub fn page_title(slug: &str) -> &'static str {{\n    match slug {{\n{}        _ => \"\",\n    }}\n}}\n",
        title_arms
    ));
    code.push_str(&format!(
        "pub fn page_toc(slug: &str) -> &'static [TocItem] {{\n    match slug {{\n{}        _ => &[],\n    }}\n}}\n",
        toc_arms
    ));

    fs::write(&dest, code).unwrap();
}

/// Strip a leading `---\n … \n---\n` YAML front-matter block.
fn strip_frontmatter(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return rest[end + 5..].to_string();
        }
        if let Some(end) = rest.find("\n---") {
            return rest[end + 4..].to_string();
        }
    }
    s.to_string()
}

/// Render markdown → HTML, assigning slug ids to h2/h3 (for anchors)
/// and collecting a table of contents. Internal `*.md` links are
/// rewritten to `/docs/<slug>` routes (or a GitHub blob URL when they
/// escape the docs tree).
fn render(md: &str, page_path: &str) -> (String, Vec<(u8, String, String)>) {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS;
    let mut events: Vec<Event> = Parser::new_ext(md, opts).collect();
    let mut toc: Vec<(u8, String, String)> = Vec::new();

    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { level, .. }) = &events[i] {
            let level = *level;
            let mut text = String::new();
            let mut j = i + 1;
            while j < events.len() {
                match &events[j] {
                    Event::Text(t) | Event::Code(t) => text.push_str(t),
                    Event::End(TagEnd::Heading(_)) => break,
                    _ => {}
                }
                j += 1;
            }
            let id = slugify(&text);
            if matches!(level, HeadingLevel::H2 | HeadingLevel::H3) {
                let depth = if level == HeadingLevel::H2 { 2 } else { 3 };
                toc.push((depth, text, id.clone()));
            }
            events[i] = Event::Start(Tag::Heading {
                level,
                id: Some(CowStr::from(id)),
                classes: Vec::new(),
                attrs: Vec::new(),
            });
        }
        i += 1;
    }

    for ev in events.iter_mut() {
        if let Event::Start(Tag::Link { dest_url, .. }) = ev {
            let rewritten = rewrite_link(dest_url, page_path);
            *dest_url = CowStr::from(rewritten);
        }
    }

    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    (out, toc)
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn rewrite_link(dest: &str, page_path: &str) -> String {
    if dest.starts_with("http") || dest.starts_with('#') || dest.starts_with("mailto:") {
        return dest.to_string();
    }
    let (path_part, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, format!("#{a}")),
        None => (dest, String::new()),
    };
    if !path_part.ends_with(".md") {
        return dest.to_string();
    }
    let mut comps: Vec<String> = vec!["docs".to_string()];
    if let Some(parent) = Path::new(page_path).parent() {
        for part in parent
            .to_string_lossy()
            .split('/')
            .filter(|s| !s.is_empty())
        {
            comps.push(part.to_string());
        }
    }
    for part in path_part.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            p => comps.push(p.to_string()),
        }
    }
    let joined = comps.join("/");
    if let Some(rel) = joined.strip_prefix("docs/") {
        let slug = rel.strip_suffix(".md").unwrap_or(rel);
        format!("/docs/{slug}{anchor}")
    } else {
        format!("https://github.com/mambisi/pocopine/blob/main/{joined}{anchor}")
    }
}
