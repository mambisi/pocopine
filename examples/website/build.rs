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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{env, fs};

use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html,
};
use quote::ToTokens;
use serde::Deserialize;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::html::highlighted_html_for_string;
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Build-time syntax highlighter. Loads syntect's bundled grammars and
/// a warm dark theme once, then renders snippets to inline-styled HTML.
/// The wasm bundle ships none of this — only the coloured markup.
struct Hl {
    ss: SyntaxSet,
    theme: Theme,
}

impl Hl {
    fn new() -> Self {
        let ss = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        // "base16-mocha.dark" is warm-toned and reads well on the
        // #2d1a0f code panels; fall back to ocean if absent.
        let theme = ts
            .themes
            .get("base16-mocha.dark")
            .or_else(|| ts.themes.get("base16-ocean.dark"))
            .cloned()
            .unwrap_or_else(|| ts.themes.values().next().cloned().unwrap());
        Self { ss, theme }
    }

    fn syntax_for(&self, lang: &str) -> Option<&SyntaxReference> {
        let lang = lang.trim().to_ascii_lowercase();
        let ext = match lang.as_str() {
            "rust" | "rs" => "rs",
            "bash" | "sh" | "shell" | "console" | "zsh" => "sh",
            "poco" | "html" | "xml" | "svg" => "html",
            "toml" => "toml",
            "css" => "css",
            "json" => "json",
            "yaml" | "yml" => "yaml",
            "js" | "javascript" | "mjs" => "js",
            "ts" | "typescript" => "ts",
            "md" | "markdown" => "md",
            "" => return None,
            other => other,
        };
        self.ss.find_syntax_by_extension(ext)
    }

    /// Highlight `code` (language `lang`) to inline-styled inner HTML —
    /// the `<span style=…>` runs only, with syntect's `<pre>` wrapper
    /// and theme background stripped so the site's panel shows through.
    fn code(&self, code: &str, lang: &str) -> String {
        let Some(syntax) = self.syntax_for(lang) else {
            return esc(code);
        };
        match highlighted_html_for_string(code, &self.ss, syntax, &self.theme) {
            Ok(html) => strip_pre(&html),
            Err(_) => esc(code),
        }
    }
}

/// Strip syntect's outer `<pre style="background:…">…</pre>`, keeping the
/// inner highlighted runs so the page's own panel background applies.
fn strip_pre(html: &str) -> String {
    let s = html.trim();
    let s = match s.find('>') {
        Some(gt) => &s[gt + 1..],
        None => s,
    };
    let s = s.strip_suffix("</pre>").unwrap_or(s);
    s.trim_matches('\n').to_string()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

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

    // ── Blog media: stage docs/assets/blog (the Remotion renders from
    // tools/blog-motion) into a served static dir, mirroring the icon
    // staging below. Pages reference them as `/assets/blog/<file>`;
    // `assets` is listed in the deploy `static_files`. Staged BEFORE
    // rendering so the asset-URL rewriter below hashes the exact bytes
    // `pocopine assets push` later scans out of `assets/`. ──
    stage_dir(
        &docs_dir.join("assets/blog"),
        &Path::new(&manifest).join("assets/blog"),
    );
    let asset_urls = AssetUrls::new(&manifest, &docs_dir);

    let hl = Hl::new();

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
            let (_, body) = parse_frontmatter(&raw);
            let body = inject_generated(&body, &manifest);
            emit_page(
                &hl,
                &asset_urls,
                &body,
                &p.path,
                slug,
                &p.title,
                &static_docs,
                &mut title_arms,
                &mut toc_arms,
            );
        }
    }
    nav.push_str("];\n");

    // ── Blog posts: every `docs/blogs/*.md` (discovered by listing the
    // directory — no manifest) renders through the same pipeline as the
    // docs pages. Fragments land in `static-docs/blogs/<slug>.html` and
    // the title/TOC tables share the `blogs/<slug>` key, so `BlogPage`
    // reuses the `page_title` / `page_toc` machinery. The small BLOGS
    // index (front-matter title / description / date) is what
    // `BlogsIndex` lists — a new post appears there automatically. ──
    code.push_str(
        "pub struct BlogPost { pub slug: &'static str, pub title: &'static str, \
         pub description: &'static str, pub date: &'static str, \
         pub date_display: &'static str }\n",
    );
    let mut posts: Vec<(String, HashMap<String, String>, String)> = Vec::new();
    if let Ok(entries) = fs::read_dir(docs_dir.join("blogs")) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(slug) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let raw = fs::read_to_string(&p).unwrap_or_default();
            let (fm, body) = parse_frontmatter(&raw);
            posts.push((slug, fm, body));
        }
    }
    // Newest first on the index; slug breaks date ties deterministically.
    posts.sort_by(|a, b| {
        let (da, db) = (a.1.get("date"), b.1.get("date"));
        db.cmp(&da).then_with(|| a.0.cmp(&b.0))
    });
    let mut blogs = String::from("pub static BLOGS: &[BlogPost] = &[\n");
    for (slug, fm, body) in &posts {
        let key = format!("blogs/{slug}");
        let title = fm.get("title").cloned().unwrap_or_else(|| slug.clone());
        let desc = fm.get("description").cloned().unwrap_or_default();
        let date = fm.get("date").cloned().unwrap_or_default();
        let body = inject_generated(body, &manifest);
        emit_page(
            &hl,
            &asset_urls,
            &body,
            &format!("{key}.md"),
            &key,
            &title,
            &static_docs,
            &mut title_arms,
            &mut toc_arms,
        );
        blogs.push_str(&format!(
            "  BlogPost {{ slug: {:?}, title: {:?}, description: {:?}, \
             date: {:?}, date_display: {:?} }},\n",
            slug,
            title,
            desc,
            date,
            human_date(&date)
        ));
    }
    blogs.push_str("];\n");

    code.push_str(&nav);
    code.push_str(&format!(
        "pub fn page_title(slug: &str) -> &'static str {{\n    match slug {{\n{}        _ => \"\",\n    }}\n}}\n",
        title_arms
    ));
    code.push_str(&format!(
        "pub fn page_toc(slug: &str) -> &'static [TocItem] {{\n    match slug {{\n{}        _ => &[],\n    }}\n}}\n",
        toc_arms
    ));
    code.push_str(&blogs);

    fs::write(&dest, code).unwrap();

    emit_snippets(&hl, &manifest, &out_dir);
}

/// Copy every regular file in `src` into `dst` (flat, non-recursive).
/// Unconditional: the staged dirs hold a handful of small media files,
/// so a fresh copy per build keeps re-renders picked up.
fn stage_dir(src: &Path, dst: &Path) {
    let Ok(entries) = fs::read_dir(src) else {
        return;
    };
    fs::create_dir_all(dst).ok();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file()
            && let Some(name) = p.file_name()
        {
            let _ = fs::copy(&p, dst.join(name));
        }
    }
}

/// RFC-100 Mode A for the build-time-rendered fragments.
///
/// The `asset!` macro resolves its base at runtime, but the docs/blog
/// fragments are static HTML rendered here — so the content-addressed
/// bucket URL is baked in at build time instead. Every
/// `src="/assets/<rel>"` in a rendered fragment becomes
/// `<public-base>/assets/<hash8>/<rel>`, which is byte-for-byte the
/// object key layout `pocopine assets push` uploads (`assets_sync.rs`
/// scans `<project>/assets`, keys `assets/<hash8>/<rel>`): the hash is
/// computed from the staged file the push will read.
///
/// Without a `public-base` in `[package.metadata.pocopine.assets]`
/// references are left on the plain staged path. The dev server also
/// serves hashed `/assets/<hash8>/…` routes, but rewriting to them
/// would make the no-bucket build diverge from the on-disk staging for
/// no gain — plain `/assets/<rel>` is served from the staged dir either
/// way, so dev stays on the unhashed path and only a configured public
/// bucket changes the emitted URLs.
struct AssetUrls {
    /// `public-base` from `[package.metadata.pocopine.assets]`,
    /// trailing-slash-trimmed. `None` → no rewriting.
    public_base: Option<String>,
    /// `<manifest>/assets` — the staged dir `pocopine assets push`
    /// scans; hashes are computed from these bytes.
    staged: PathBuf,
    /// `docs/assets` — the staging *source*. rerun-if-changed points
    /// here (the staged copy is rewritten by this script every build,
    /// so tracking it would re-dirty the build each run).
    source: PathBuf,
}

impl AssetUrls {
    fn new(manifest: &str, docs_dir: &Path) -> Self {
        let cargo_toml = Path::new(manifest).join("Cargo.toml");
        println!("cargo:rerun-if-changed={}", cargo_toml.display());
        let public_base = fs::read_to_string(&cargo_toml)
            .ok()
            .and_then(|s| s.parse::<toml::Value>().ok())
            .and_then(|v| {
                Some(
                    v.get("package")?
                        .get("metadata")?
                        .get("pocopine")?
                        .get("assets")?
                        .get("public-base")?
                        .as_str()?
                        .trim_end_matches('/')
                        .to_string(),
                )
            })
            .filter(|base| !base.is_empty());
        Self {
            public_base,
            staged: Path::new(manifest).join("assets"),
            source: docs_dir.join("assets"),
        }
    }

    /// Rewrite every `src="/assets/<rel>"` in a rendered fragment to
    /// the hashed bucket URL. References whose staged file is missing
    /// are left untouched (they would 404 either way; the plain path
    /// at least names the intended file).
    fn rewrite(&self, html: &str) -> String {
        const NEEDLE: &str = "src=\"/assets/";
        let Some(base) = &self.public_base else {
            return html.to_string();
        };
        let mut out = String::with_capacity(html.len());
        let mut rest = html;
        while let Some(at) = rest.find(NEEDLE) {
            out.push_str(&rest[..at]);
            let tail = &rest[at + NEEDLE.len()..];
            let Some(end) = tail.find('"') else {
                // Unterminated attribute — emit the tail verbatim.
                rest = &rest[at..];
                break;
            };
            let rel = &tail[..end];
            match self.hashed_url(base, rel) {
                Some(url) => out.push_str(&format!("src=\"{url}\"")),
                None => {
                    out.push_str(NEEDLE);
                    out.push_str(rel);
                    out.push('"');
                }
            }
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// `blog/clip.webm` → `<base>/assets/<hash8>/blog/clip.webm`, or
    /// `None` when the path is suspicious or the staged file is absent.
    fn hashed_url(&self, base: &str, rel: &str) -> Option<String> {
        if rel.is_empty()
            || rel
                .split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return None;
        }
        let bytes = fs::read(self.staged.join(rel)).ok()?;
        // 8-hex sha256 prefix — the shared asset-hash shape
        // (`pocopine_core::assets::asset_hash`; the runtime crate is
        // not a build-dependency, so the 2-line prefix is reproduced
        // here the same way `assets_sync.rs` does in the CLI).
        let mut hash = pocopine_crypto::sha256_hex(&bytes);
        hash.truncate(8);
        // Rebuild when the staging *source* changes (see field doc).
        let source = self.source.join(rel);
        if source.is_file() {
            println!("cargo:rerun-if-changed={}", source.display());
        }
        Some(format!("{base}/assets/{hash}/{rel}"))
    }
}

/// Pre-highlight the home-page showcase snippets into `gen_code.rs`.
/// The raw snippets live here (the build host) so syntect can colour
/// them once; the component consumes the finished HTML via `pp-html`.
fn emit_snippets(hl: &Hl, manifest: &str, out_dir: &str) {
    let mut s = String::new();
    s.push_str("pub mod showcase {\n");
    s.push_str(
        "    pub struct Feat {\n\
         \x20       pub name: &'static str,\n\
         \x20       pub file: &'static str,\n\
         \x20       pub lang: &'static str,\n\
         \x20       pub title: &'static str,\n\
         \x20       pub desc: &'static str,\n\
         \x20       pub doc: &'static str,\n\
         \x20       pub code_html: &'static str,\n\
         \x20   }\n",
    );
    s.push_str("    pub static FEATS: &[Feat] = &[\n");
    for f in SHOWCASE_FEATS {
        let html = hl.code(f.code, f.lang);
        s.push_str(&format!(
            "        Feat {{ name: {:?}, file: {:?}, lang: {:?}, title: {:?}, desc: {:?}, doc: {:?}, code_html: {:?} }},\n",
            f.name, f.file, f.lang, f.title, f.desc, f.doc, html
        ));
    }
    s.push_str("    ];\n}\n");

    // Secure-section snippets, highlighted by key.
    s.push_str("pub mod secure {\n");
    s.push_str("    pub fn code(key: &str) -> &'static str {\n        match key {\n");
    for (key, lang, code) in SECURE_SNIPPETS {
        let html = hl.code(code, lang);
        s.push_str(&format!("            {key:?} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n}\n");

    // "Whole flow" walkthrough snippets, highlighted by step index.
    s.push_str("pub mod flow {\n");
    s.push_str("    pub fn code(i: usize) -> &'static str {\n        match i {\n");
    for (idx, (lang, code)) in FLOW_SNIPPETS.iter().enumerate() {
        let html = hl.code(code, lang);
        s.push_str(&format!("            {idx} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n}\n");

    // Charts-page snippets (data setup + per-chart markup), by key.
    s.push_str("pub mod charts {\n");
    s.push_str("    pub fn code(key: &str) -> &'static str {\n        match key {\n");
    for (key, lang, code) in CHARTS_SNIPPETS {
        let html = hl.code(code, lang);
        s.push_str(&format!("            {key:?} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n}\n");

    // Motion-page snippets (per-technique pine-motion calls), by key.
    s.push_str("pub mod motion {\n");
    s.push_str("    pub fn code(key: &str) -> &'static str {\n        match key {\n");
    for (key, lang, code) in MOTION_SNIPPETS {
        let html = hl.code(code, lang);
        s.push_str(&format!("            {key:?} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n}\n");

    // Component reference Code tabs: every showcase demo's `.poco`
    // source, highlighted as markup. The slug is the demo directory
    // with `_` → `-` (matching `component_meta`), so the table stays in
    // sync with the demos on disk without a duplicated path list.
    let showcase = Path::new(manifest).join("src/components/showcase");
    println!("cargo:rerun-if-changed={}", showcase.display());
    let mut demos: Vec<(String, String)> = Vec::new();
    // Per-component install snippet (correct `.register::<…>()` calls
    // derived from the demo's own `pine-{slug}` tags), highlighted.
    let mut installs: Vec<(String, String)> = Vec::new();
    if let Ok(dirs) = fs::read_dir(&showcase) {
        for dir in dirs.flatten() {
            if !dir.path().is_dir() {
                continue;
            }
            let slug = dir.file_name().to_string_lossy().replace('_', "-");
            let Ok(files) = fs::read_dir(dir.path()) else {
                continue;
            };
            for file in files.flatten() {
                let fname = file.file_name().to_string_lossy().into_owned();
                if fname.ends_with("Demo.poco") {
                    let src = fs::read_to_string(file.path()).unwrap_or_default();
                    demos.push((slug.clone(), hl.code(src.trim_end_matches('\n'), "poco")));
                    installs.push((slug.clone(), hl.code(&install_snippet(&src, &slug), "rust")));
                    break;
                }
            }
        }
    }
    demos.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic output
    s.push_str("pub mod component {\n");
    s.push_str("    pub fn code(slug: &str) -> &'static str {\n        match slug {\n");
    for (slug, html) in &demos {
        s.push_str(&format!("            {slug:?} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n}\n");

    // Auto-generated reference data: props (from the Pine `#[prop]` /
    // `#[model]` fields), the per-component install snippet, and the
    // anatomy skeleton (highlighted from the `component_meta` strings).
    let pine_api = extract_pine_api(manifest);
    let meta_anatomy = parse_meta_anatomy(manifest);
    let mut props_arms = String::new();
    let mut install_arms = String::new();
    let mut anatomy_arms = String::new();
    for (slug, _) in &demos {
        let exact = format!("pine-{slug}");
        let sub = format!("pine-{slug}-");
        let mut props: Vec<&ApiPropRaw> = pine_api
            .iter()
            .filter(|p| p.tag == exact || p.tag.starts_with(&sub))
            .collect();
        props.sort_by(|a, b| a.tag.cmp(&b.tag)); // stable: keeps field order within a tag
        if !props.is_empty() {
            props_arms.push_str(&format!("            {slug:?} => &[\n"));
            for p in props {
                props_arms.push_str(&format!(
                    "                ApiProp {{ key: {:?}, element: {:?}, name: {:?}, ty: {:?}, desc: {:?} }},\n",
                    format!("{}.{}", p.tag, p.name), p.tag, p.name, p.ty, p.doc
                ));
            }
            props_arms.push_str("            ],\n");
        }
        if let Some((_, html)) = installs.iter().find(|(s, _)| s == slug) {
            install_arms.push_str(&format!("            {slug:?} => {html:?},\n"));
        }
        if let Some(anatomy) = meta_anatomy.get(slug)
            && !anatomy.is_empty()
        {
            anatomy_arms.push_str(&format!(
                "            {slug:?} => {:?},\n",
                hl.code(anatomy, "poco")
            ));
        }
    }
    s.push_str("pub mod api {\n");
    s.push_str(
        "    pub struct ApiProp { pub key: &'static str, pub element: &'static str, \
         pub name: &'static str, pub ty: &'static str, pub desc: &'static str }\n",
    );
    s.push_str(&format!(
        "    pub fn props_for(slug: &str) -> &'static [ApiProp] {{\n        match slug {{\n{props_arms}            _ => &[],\n        }}\n    }}\n"
    ));
    s.push_str(&format!(
        "    pub fn install_for(slug: &str) -> &'static str {{\n        match slug {{\n{install_arms}            _ => \"\",\n        }}\n    }}\n"
    ));
    s.push_str(&format!(
        "    pub fn anatomy_for(slug: &str) -> &'static str {{\n        match slug {{\n{anatomy_arms}            _ => \"\",\n        }}\n    }}\n"
    ));
    s.push_str("}\n");

    // ── Icons page: enumerate the Tabler catalogue + stage the SVG
    // assets into a served static dir. Names go into the wasm for
    // instant client-side search; the SVGs are streamed from the server
    // (mask-rendered so they follow the theme). ──
    let tabler = Path::new(manifest).join("../../crates/pine-icons/assets/tabler");
    println!(
        "cargo:rerun-if-changed={}",
        tabler.join("MANIFEST.json").display()
    );
    let outline = icon_names(&tabler.join("outline"));
    let filled = icon_names(&tabler.join("filled"));
    s.push_str("pub mod icons {\n");
    s.push_str(&format!(
        "    pub static OUTLINE: &str = {:?};\n",
        outline.join("\n")
    ));
    s.push_str(&format!(
        "    pub static FILLED: &str = {:?};\n",
        filled.join("\n")
    ));
    // Install / usage / macro snippets, highlighted by key.
    s.push_str("    pub fn code(key: &str) -> &'static str {\n        match key {\n");
    for (key, lang, code) in ICONS_SNIPPETS {
        let html = hl.code(code, lang);
        s.push_str(&format!("            {key:?} => {html:?},\n"));
    }
    s.push_str("            _ => \"\",\n        }\n    }\n");
    s.push_str("}\n");
    stage_icons(
        &tabler.join("outline"),
        &Path::new(manifest).join("icons/outline"),
        outline.len(),
    );
    stage_icons(
        &tabler.join("filled"),
        &Path::new(manifest).join("icons/filled"),
        filled.len(),
    );

    let dest = Path::new(out_dir).join("gen_code.rs");
    fs::write(dest, s).unwrap();
}

/// Collect kebab-case icon names (SVG file stems) from a Tabler asset
/// directory, sorted.
fn icon_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("svg") {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Copy a Tabler asset directory's SVGs into a served static dir. Skips
/// the whole copy when the destination already holds the expected count,
/// so a docs-only rebuild doesn't re-copy thousands of files.
fn stage_icons(src: &Path, dst: &Path, expected: usize) {
    let have = fs::read_dir(dst).map(|d| d.flatten().count()).unwrap_or(0);
    if expected > 0 && have == expected {
        return;
    }
    fs::create_dir_all(dst).ok();
    if let Ok(entries) = fs::read_dir(src) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("svg")
                && let Some(name) = p.file_name()
            {
                let _ = fs::copy(&p, dst.join(name));
            }
        }
    }
}

/// One auto-extracted prop: element tag, field name, type, doc comment.
struct ApiPropRaw {
    tag: String,
    name: String,
    ty: String,
    doc: String,
}

/// Parse the Pine component sources and pull every `#[prop]` / `#[model]`
/// field (type + doc comment) off each `#[component]` struct. The
/// element tag is the struct name kebab-cased (`PineDialogRoot` →
/// `pine-dialog-root`).
fn extract_pine_api(manifest: &str) -> Vec<ApiPropRaw> {
    let root = Path::new(manifest).join("../../crates/pine/src");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    let mut out = Vec::new();
    for path in files {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&src) else {
            continue;
        };
        for item in file.items {
            let syn::Item::Struct(s) = item else {
                continue;
            };
            if !has_attr(&s.attrs, "component") {
                continue;
            }
            let tag = kebab(&s.ident.to_string());
            let syn::Fields::Named(named) = s.fields else {
                continue;
            };
            for f in named.named {
                if !has_attr(&f.attrs, "prop") && !has_attr(&f.attrs, "model") {
                    continue;
                }
                let Some(name) = f.ident.as_ref().map(|i| i.to_string()) else {
                    continue;
                };
                out.push(ApiPropRaw {
                    tag: tag.clone(),
                    name,
                    ty: ty_str(&f.ty),
                    doc: doc_of(&f.attrs),
                });
            }
        }
    }
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn has_attr(attrs: &[syn::Attribute], ident: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(ident))
}

fn doc_of(attrs: &[syn::Attribute]) -> String {
    let mut lines = Vec::new();
    for a in attrs {
        if !a.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &a.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            lines.push(s.value().trim().to_string());
        }
    }
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ty_str(ty: &syn::Type) -> String {
    let s = ty.to_token_stream().to_string();
    s.replace(" <", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace("> ", ">")
        .replace(" ::", "::")
        .replace(":: ", "::")
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `pine-dialog-root` → `PineDialogRoot`.
fn struct_name(tag: &str) -> String {
    tag.split('-')
        .map(|seg| {
            let mut ch = seg.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().chain(ch).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// Build the per-component install snippet from the `pine-{slug}` tags
/// the demo actually uses — a correct `.register::<…>()` chain (not
/// `register_all()`, which would pull in every primitive). Falls back
/// to `register_all()` only when no `pine-{slug}` element is found.
fn install_snippet(src: &str, slug: &str) -> String {
    let exact = format!("pine-{slug}");
    let sub = format!("pine-{slug}-");
    let mut tags: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(off) = src[i..].find('<') {
        let start = i + off + 1;
        i = start;
        let tag: String = src[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if (tag == exact || tag.starts_with(&sub)) && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    if tags.is_empty() {
        return "// cargo add pocopine pine\npine::register_all(); // every Pine primitive".into();
    }
    // Root element first, then the rest alphabetically.
    tags.sort();
    tags.sort_by_key(|t| (!(*t == exact || t.ends_with("-root")), t.clone()));
    let mut out = String::from("// cargo add pocopine pine\nApp::new()\n");
    for t in &tags {
        out.push_str(&format!("    .register::<pine::{}>()\n", struct_name(t)));
    }
    out.push_str("    .run();");
    out
}

/// Parse `component_meta.rs` and pull each entry's `slug` → `anatomy`
/// string, so the anatomy skeleton can be highlighted at build time
/// without duplicating it (the meta stays the single source).
fn parse_meta_anatomy(manifest: &str) -> HashMap<String, String> {
    let path = Path::new(manifest).join("src/components/component_meta.rs");
    println!("cargo:rerun-if-changed={}", path.display());
    let mut map = HashMap::new();
    let Ok(src) = fs::read_to_string(&path) else {
        return map;
    };
    let Ok(file) = syn::parse_file(&src) else {
        return map;
    };
    for item in file.items {
        let syn::Item::Const(c) = item else { continue };
        if c.ident != "COMPONENTS" {
            continue;
        }
        let array = match &*c.expr {
            syn::Expr::Reference(r) => &*r.expr,
            other => other,
        };
        let syn::Expr::Array(arr) = array else {
            continue;
        };
        for elem in &arr.elems {
            let syn::Expr::Struct(st) = elem else {
                continue;
            };
            let mut slug = None;
            let mut anatomy = None;
            for f in &st.fields {
                let syn::Member::Named(key) = &f.member else {
                    continue;
                };
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(ls),
                    ..
                }) = &f.expr
                {
                    match key.to_string().as_str() {
                        "slug" => slug = Some(ls.value()),
                        "anatomy" => anatomy = Some(ls.value()),
                        _ => {}
                    }
                }
            }
            if let (Some(s), Some(a)) = (slug, anatomy) {
                map.insert(s, a);
            }
        }
    }
    map
}

/// Secure-section snippets `(key, language, source)`. Highlighted at
/// build time; `SecureSection` injects them via `pp-html`.
const SECURE_SNIPPETS: &[(&str, &str, &str)] = &[
    (
        "client",
        "rust",
        "// One plugin carries the bearer token + sign-in state.\nApp::new()\n    .plugin(auth_plugin().login_route(\"/login\"))\n    .route::<Dashboard>(\"/dashboard\")\n    .run();\n\n// A guard gates the page before it mounts,\n// reusing the shared `members_only` predicate.\nimpl RouteComponent for Dashboard {\n    fn config() -> RouteConfig<Self> {\n        RouteConfig::new()\n            .guard(predicate_guard(members_only))\n    }\n}",
    ),
    (
        "server",
        "rust",
        "// Verify every request's token into a Principal.\nServer::new(router)\n    .with_auth(JwtVerifier::firebase(project))\n    .plugin(Credentials::new(users))\n    .serve(\"0.0.0.0:3000\")\n    .await?;\n\n// The SAME predicate — checked before the body is parsed.\n#[server(guard = members_only)]\nasync fn close_account(id: Uuid) -> ServerResult<()> {\n    db().close(id).await?;\n    Ok(())\n}",
    ),
    (
        "observe",
        "rust",
        "// Same call in the browser and on the server.\ntracing::info!(\n    target: \"pocopine.log\",\n    user = %principal.id(),   // privacy-labeled field\n    route = \"/checkout\",\n    \"checkout completed\",\n);\n// one event → logging · OpenTelemetry traces · analytics",
    ),
];

/// "One language, the whole flow" walkthrough snippets `(language,
/// source)`, in step order. Highlighted at build time; `StackFlow`
/// injects them via `pp-html`.
const FLOW_SNIPPETS: &[(&str, &str)] = &[
    (
        "rust",
        "#[component(template = \"IssueList.poco\")]\npub struct IssueList {\n    issues: Vec<Issue>,\n    draft: String,\n}",
    ),
    (
        "rust",
        "// one async fn → a server route AND a typed client stub\n#[pocopine::server]\nasync fn list_issues() -> ServerResult<Vec<Issue>> {\n    db().open_issues().await\n}\n\n// in the component: let open = list_issues().await?;",
    ),
    (
        "rust",
        "// local-first view; updates push to every client\nlet open = Issue::query()\n    .eq(field::status, Status::Open)\n    .observe();",
    ),
    (
        "rust",
        "Server::new(router)\n    .with_auth(JwtVerifier::firebase(project))\n    .plugin(Jobs::redis(url))        // background work\n    .plugin(Storage::s3(bucket));    // uploads",
    ),
    (
        "rust",
        "tracing::info!(target: \"pocopine.log\", route = \"/issues\", \"served\");\n// then ship it:\n//   $ pocopine deploy   → web + worker, one container",
    ),
];

/// Charts-page snippets — the data setup (Rust) and per-chart markup
/// (`.poco`). Highlighted at build time; `ChartsPage` injects via
/// `pp-html`.
const CHARTS_SNIPPETS: &[(&str, &str, &str)] = &[
    (
        "setup",
        "rust",
        "use pine_charts::{ChartLineSeries, ChartPoint, line_legend_items};\n\n// Plain Rust vectors — built in Default or on_setup:\nlet line_series = vec![\n    ChartLineSeries::new(\"Actual\", actual_points),\n    ChartLineSeries::new(\"Target\", target_points),\n];\nlet line_legend = line_legend_items(&line_series);",
    ),
    (
        "line",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1.6\" min_height=\"220\">\n  <pine-line-chart label=\"Weekly growth\"\n                   pp-bind:series=\"line_series\"\n                   x_label=\"Week\" y_label=\"Value\"\n                   animate=\"true\"></pine-line-chart>\n</pine-chart-responsive>\n<pine-chart-legend pp-bind:items=\"line_legend\"></pine-chart-legend>",
    ),
    (
        "bar",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1.6\" min_height=\"220\">\n  <pine-bar-chart label=\"Distribution\"\n                  pp-bind:series=\"bar_series\"\n                  x_label=\"Week\" y_label=\"Count\"\n                  animate=\"true\"></pine-bar-chart>\n</pine-chart-responsive>\n<pine-chart-legend pp-bind:items=\"bar_legend\"></pine-chart-legend>",
    ),
    (
        "area",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1.6\" min_height=\"220\">\n  <pine-area-chart label=\"Trend\"\n                   pp-bind:series=\"area_series\"\n                   x_label=\"Week\" y_label=\"Volume\"\n                   show_markers=\"true\" animate=\"true\"></pine-area-chart>\n</pine-chart-responsive>\n<pine-chart-legend pp-bind:items=\"area_legend\"></pine-chart-legend>",
    ),
    (
        "pie",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1\" min_height=\"220\">\n  <pine-pie-chart label=\"Channel share\"\n                  pp-bind:data=\"pie_data\"\n                  animate=\"true\"></pine-pie-chart>\n</pine-chart-responsive>\n<pine-chart-legend pp-bind:items=\"pie_legend\"></pine-chart-legend>",
    ),
    (
        "scatter",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1.6\" min_height=\"220\">\n  <pine-scatter-chart label=\"Spend vs. reach\"\n                      pp-bind:series=\"scatter_series\"\n                      x_label=\"Spend\" y_label=\"Reach\"\n                      animate=\"true\"></pine-scatter-chart>\n</pine-chart-responsive>",
    ),
    (
        "radial",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1\" min_height=\"240\">\n  <pine-radial-bar-chart label=\"Readiness rings\"\n                         pp-bind:data=\"radial_data\"\n                         inner_radius=\"0.34\" ring_gap=\"7\"\n                         center_label=\"Avg\" pp-bind:center_value=\"radial_center\"\n                         animate=\"true\"></pine-radial-bar-chart>\n</pine-chart-responsive>",
    ),
    (
        "donut",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1\" min_height=\"240\">\n  <pine-pie-chart label=\"Channel share\"\n                  pp-bind:data=\"pie_data\"\n                  inner_radius=\"0.6\"\n                  center_label=\"Channels\" center_value=\"4\"\n                  animate=\"true\"></pine-pie-chart>\n</pine-chart-responsive>",
    ),
    (
        "stacked-bar",
        "poco",
        "<pine-chart-responsive aspect_ratio=\"1.7\" min_height=\"240\">\n  <pine-bar-chart label=\"Distribution\"\n                  pp-bind:series=\"bar_series\"\n                  mode=\"stacked\"\n                  x_label=\"Week\" y_label=\"Count\"\n                  animate=\"true\"></pine-bar-chart>\n</pine-chart-responsive>",
    ),
    (
        "cartesian",
        "poco",
        "<!-- Compose series + reference overlays on shared axes. -->\n<pine-cartesian-chart label=\"Weekly metric\" animate=\"true\">\n  <pine-chart-grid></pine-chart-grid>\n  <pine-x-axis label=\"Week\"></pine-x-axis>\n  <pine-y-axis label=\"Metric\"></pine-y-axis>\n  <pine-bar-series label=\"Actual\" color=\"#16a085\" pp-bind:data=\"bar_data\"></pine-bar-series>\n  <pine-line-series label=\"Target\" color=\"#d96c2c\" show_markers=\"true\" pp-bind:data=\"line_data\"></pine-line-series>\n  <pine-cartesian-reference-line label=\"Goal\" pp-bind:y=\"goal\" stroke_dasharray=\"4 4\"></pine-cartesian-reference-line>\n</pine-cartesian-chart>",
    ),
    (
        "layer",
        "poco",
        "<!-- Hand-place layers for custom diagrams (here, a metro map). -->\n<pine-layer-chart label=\"Metro\" animate=\"true\">\n  <pine-chart-layer name=\"series\">\n    <pine-chart-line color=\"#19a974\" stroke_width=\"14\" pp-bind:points=\"line_a\"></pine-chart-line>\n  </pine-chart-layer>\n  <pine-chart-layer name=\"markers\">\n    <pine-chart-marker label=\"Central\" x=\"420\" y=\"180\" radius=\"9\"\n                       fill=\"#19a974\" stroke=\"#fff\" stroke_width=\"3\"></pine-chart-marker>\n  </pine-chart-layer>\n</pine-layer-chart>",
    ),
    (
        "install",
        "rust",
        "// Cargo.toml\n//   pine-charts = \"0.1\"\n\n// Charts render to SVG. Register the custom elements once at startup:\nfn main() {\n    pine_charts::register_all();\n    App::new().register::<Dashboard>().run();\n}",
    ),
];

/// Motion-page snippets — one pine-motion call per technique.
/// Highlighted at build time; `MotionPage` injects via `pp-html`.
const MOTION_SNIPPETS: &[(&str, &str, &str)] = &[
    (
        "spring",
        "rust",
        "use pine_motion::{animate, Spring};\n\n// Spring presets are the react-motion / flip-toolkit names.\nanimate(&el, &[(\"transform\", \"scale(0.85)\", \"scale(1)\")], Spring::gentle());\n// also: Spring::stiff(), Spring::wobbly()",
    ),
    (
        "stagger",
        "rust",
        "use pine_motion::{animate, Stagger, Origin, Tween};\n\nlet stagger = Stagger::new(60.0).from(Origin::Center);\nfor (i, el) in cells.iter().enumerate() {\n    let delay = stagger.delay_for(i, cells.len());\n    animate(el, &[(\"opacity\", \"0\", \"1\")], Tween::new().duration(420).delay(delay));\n}",
    ),
    (
        "drag",
        "rust",
        "use pine_motion::{drag, DragConfig, DragAxis, Spring};\n\nlet handle = drag(&card, DragConfig {\n    axis: DragAxis::Both,\n    momentum: true,\n    release_spring: Spring::gentle(),\n    ..Default::default()\n});",
    ),
    (
        "state",
        "rust",
        "use pine_motion::{animate, Spring};\n\n// Animate to a target whenever state changes — the box springs\n// from its current transform to the new one.\nanimate(&el, &[(\"transform\", &from, &to)], Spring::wobbly());",
    ),
    (
        "install",
        "rust",
        "// Cargo.toml\n//   pine-motion = \"0.1\"\n\n// pine-motion is imperative — nothing to register. Call it from a\n// #[handlers] method with an element resolved from a ref:\nuse pine_motion::{animate, Spring};\n\nanimate(&el, &[(\"transform\", \"scale(0.9)\", \"scale(1)\")], Spring::gentle());",
    ),
    (
        "easing",
        "rust",
        "use pine_motion::{animate, Tween, Easing};\n\n// Named cubic-bezier presets — or pass your own control points.\nanimate(&el, &[(\"opacity\", \"0\", \"1\")],\n    Tween::new().duration(320).easing(Easing::EASE_OUT_EXPO));\nanimate(&el, &[(\"transform\", \"translateY(8px)\", \"none\")],\n    Tween::new().easing(Easing::CubicBezier([0.2, 0.8, 0.2, 1.0])));",
    ),
    (
        "hover",
        "rust",
        "use pine_motion::{raise, scale};\n\n// Lift + shadow on hover — or scale(&card, 1.06) for a grow.\nlet _hover = raise(&card);",
    ),
    (
        "focus",
        "rust",
        "use pine_motion::{focus, animate, Spring};\n\nlet _focus = focus(&input, move |focused, _| {\n    let (from, to) = if focused {\n        (\"0 0 0 0\", \"0 0 0 3px rgba(59,130,246,0.45)\")\n    } else {\n        (\"0 0 0 3px rgba(59,130,246,0.45)\", \"0 0 0 0\")\n    };\n    animate(&input, &[(\"boxShadow\", from, to)], Spring::gentle());\n});",
    ),
    (
        "press",
        "rust",
        "use pine_motion::{press, animate, Spring};\n\nlet _press = press(&btn, move |_| {\n    animate(&btn, &[(\"transform\", \"scale(1)\", \"scale(0.92)\")], Spring::stiff());\n    Some(Box::new(move |_, _| {\n        animate(&btn, &[(\"transform\", \"scale(0.92)\", \"scale(1)\")], Spring::wobbly());\n    }))\n});",
    ),
    (
        "tilt",
        "rust",
        "use pine_motion::{tilt, TiltConfig};\n\n// Pointer position rotates the card in 3D; children marked\n// `data-pm-tilt` lift on hover.\nlet _tilt = tilt(&card, TiltConfig { divisor: 18.0, transition_ms: 120.0 });",
    ),
    (
        "scroll",
        "rust",
        "use pine_motion::scroll_progress;\n\n// Calls back with 0..=1 page progress on every scroll —\n// e.g. fill a reading-progress bar:\nlet _p = scroll_progress(|t| {\n    set_bar_width(t * 100.0);\n});",
    ),
    (
        "inview",
        "rust",
        "use pine_motion::{on_view, animate, ViewConfig, Spring};\n\n// Fire once when the element scrolls into view — fade + rise it in:\nlet _v = on_view(&box_el, ViewConfig { threshold: 0.25, once: true }, move |shown| {\n    if shown {\n        animate(&box_el, &[(\"opacity\", \"0\", \"1\"),\n                          (\"transform\", \"translateY(24px)\", \"translateY(0px)\")],\n            Spring::gentle());\n    }\n});",
    ),
    (
        "pan",
        "rust",
        "use pine_motion::{pan, PanConfig, PanEvent};\n\nlet _pan = pan(&zone, PanConfig::default(), move |ev| match ev {\n    PanEvent::Move { delta, velocity } => {\n        // delta.0 / delta.1 cumulative; velocity.0 / .1 instantaneous\n    }\n    PanEvent::End { velocity } => { /* hand to Spring::with_velocity */ }\n    _ => {}\n});",
    ),
    (
        "layout",
        "rust",
        "use pine_motion::{snapshot_layout, play_layout, Spring};\n\n// FLIP: snapshot, mutate the DOM, then play the delta. Mark tracked\n// nodes with `data-pp-layout-id`.\nlet before = snapshot_layout(&list);\nreorder(&list);\nplay_layout(&list, before, Spring::gentle());",
    ),
];

/// Icons-page snippets — install + template + macro usage. Highlighted
/// at build time; `IconsPage` injects via `pp-html`.
const ICONS_SNIPPETS: &[(&str, &str, &str)] = &[
    (
        "install",
        "rust",
        "// Cargo.toml\n//   pine-icons = \"0.1\"\n\n// Register the <pine-icon> primitive + the names you use. Only the\n// listed icons are pulled into the wasm — tree-shaking by default.\nfn main() {\n    pine_icons::register_icons![\"search\", \"user\", \"chevron-down\"];\n    App::new().register::<pine_icons::PineIcon>().run();\n}",
    ),
    (
        "template",
        "poco",
        "<!-- A registered icon. Color follows `currentColor`. -->\n<pine-icon name=\"user\" size=\"20\"></pine-icon>\n<pine-icon name=\"star\" variant=\"filled\" size=\"16\"></pine-icon>\n\n<!-- Bind the name reactively — one element, not two pp-show'd icons. -->\n<pine-icon :name=\"open ? 'chevron-up' : 'chevron-down'\"></pine-icon>",
    ),
    (
        "macro",
        "rust",
        "use pine_icons::icon;\n\n// Compile-time inline SVG — no registry, no lookup. &'static str.\nconst SEARCH: &str = icon!(outline / \"search\");\nlet star = icon!(filled / \"star\");",
    ),
];

/// Source data for the showcase. Highlighted at build time (see
/// `emit_snippets`); the component reads the generated `FEATS`.
struct ShowcaseFeat {
    name: &'static str,
    file: &'static str,
    lang: &'static str,
    title: &'static str,
    desc: &'static str,
    doc: &'static str,
    code: &'static str,
}

const SHOWCASE_FEATS: &[ShowcaseFeat] = &[
    ShowcaseFeat {
        name: "Components",
        file: "Todo.rs",
        lang: "rust",
        title: "Components in plain Rust",
        desc: "A component is a struct and a template — inline as poco! while it is small, in its own .poco file once it grows, and `pocopine fmt` moves it across that line for you. State is just fields; handlers mutate &mut self. No virtual DOM and no Rc<RefCell> — pocopine makes the fields reactive and updates the real DOM in place.",
        doc: "/docs/guides/components/README",
        code: "#[derive(Default, Serialize, Deserialize)]\n#[component(template = poco! {\n    <input pp-model=\"draft\">\n    <button @click=\"add\">Add</button>\n})]\npub struct TodoApp {\n    items: Vec<Todo>,\n    draft: String,\n}\n\n#[handlers]\nimpl TodoApp {\n    pub fn add(&mut self) {\n        self.items.push(Todo::new(&self.draft));\n        self.draft.clear();\n    }\n}",
    },
    ShowcaseFeat {
        name: "Stylekit",
        file: "Button.poco",
        lang: "poco",
        title: "Utility CSS that compiles itself",
        desc: "Pine Stylekit is a built-in, Tailwind-shaped utility-CSS compiler — on by default in build, run, and dev. It parses your .poco (it doesn't scan), validates every class against the token catalog, and emits one deterministic stylesheet. No watcher, no config; an unknown class is a build error, not a silent miss.",
        doc: "/docs/guides/styling/stylekit",
        code: "<!-- Tailwind-shaped utilities, compiled at build time -->\n<button class=\"inline-flex items-center gap-2 px-3.5 py-2\n               rounded-md bg-accent text-surface font-medium\n               hover:bg-accent-strong transition-colors\">\n  Ship it\n</button>\n<!-- bg-accent → var(--color-accent); unknown class = build error -->",
    },
    ShowcaseFeat {
        name: "Server functions",
        file: "api.rs",
        lang: "rust",
        title: "Call the server like a function",
        desc: "Mark an async fn #[server] and it compiles to a backend route plus a typed client stub. You call it from the browser as a normal async fn — same language, same types, both ends. Return a stream instead of a value and it streams over SSE, item by item, with no extra attribute and cancellation for free. No fetch glue, no hand-written endpoints.",
        doc: "/docs/guides/server/server-plugins",
        code: "// one function, two build targets\n#[pocopine::server]\nasync fn close(id: Uuid) -> ServerResult<()> {\n    db().close_issue(id).await?;\n    Ok(())\n}\n\n// the client calls close(id).await like any async fn",
    },
    ShowcaseFeat {
        name: "Data & live",
        file: "issues.rs",
        lang: "rust",
        title: "Local-first data, live-synced",
        desc: "Query data with #[query_resource]: reactive, offline-first views that update optimistically and sync in the background. Live invalidation pushes each change to every connected client automatically, and when clients need to edit the same document at once, the realtime gateway carries CRDT updates over one WebSocket.",
        doc: "/docs/guides/data/sync-client",
        code: "// local-first, reactive, live-synced\n#[query_resource]\nstruct Issues;\n\nlet open = Issues::query(&client)\n    .filter(|i| i.open)\n    .observe();      // updates push to every client",
    },
    ShowcaseFeat {
        name: "Auth & services",
        file: "main.rs",
        lang: "rust",
        title: "Auth, storage, jobs — as plugins",
        desc: "Drop in email + password or JWT auth (Firebase, Clerk, Auth0, Supabase), object storage with server-mediated uploads, and Redis-backed background jobs. Each installs as a server plugin in one place.",
        doc: "/docs/guides/auth/credentials",
        code: "app! {\n    plugins: [\n        Credentials::new(users),        // email + password\n        JwtVerifier::firebase(project),  // Clerk / Auth0 / Supabase\n        Storage::s3(bucket),             // uploads\n        Jobs::redis(url),                // background jobs\n    ],\n}",
    },
    ShowcaseFeat {
        name: "Deploy",
        file: "shell",
        lang: "bash",
        title: "Ship with one command",
        desc: "`pocopine deploy` builds an image and deploys the web and worker processes to Railway, Render, or Cloudflare Pages through their APIs — no host CLIs. Or run the very same container anywhere Docker runs.",
        doc: "/docs/getting-started/introduction",
        code: "$ pocopine build --release\n$ pocopine deploy\n  ✓ web + worker → railway\n  → https://app.up.railway.app",
    },
    ShowcaseFeat {
        name: "AI",
        file: "summarize.rs",
        lang: "rust",
        title: "AI flows that are typed and traced",
        desc: "Agenkit makes an LLM call ordinary Rust: a tool is a function, a flow is a function, and the output is validated against your own struct instead of parsed and hoped over. Anthropic, any OpenAI-compatible endpoint, and Qwen ship as providers, with model names the compiler checks. Every run emits one trace tree, and what a caller may see is a ceiling the flow declares.",
        doc: "/docs/guides/agenkit/README",
        code: "#[ai_flow(tools(\"word_count\"))]\nasync fn summarize(input: Prompt, ctx: AiFlowContext)\n    -> AgenkitResult<Summary>\n{\n    ctx.ai()\n        .system(\"Summarize the prompt.\")\n        .prompt(input.text)\n        .schema::<Summary>()   // your struct, validated\n        .generate_structured()\n        .await\n}",
    },
    ShowcaseFeat {
        name: "Observability",
        file: "checkout.rs",
        lang: "rust",
        title: "Observability, built in",
        desc: "One structured-event contract feeds logging, OpenTelemetry tracing, and analytics sinks — the same API in the browser and on the server, with privacy labels so sensitive fields never leak into logs. Spans hang from one trunk, so a request, the job it queued, and the model call it made read as a single trace.",
        doc: "/docs/guides/observability/logging-tracing",
        code: "tracing::info!(\n    target: \"pocopine.log\",\n    user = %id,\n    \"checkout completed\",\n);\n// one event → logging · OTLP tracing · analytics",
    },
];

/// Split a leading `---\n … \n---\n` YAML front-matter block off a
/// markdown document. Returns the front-matter as a flat key → value
/// map (values may be double-quoted) plus the body. The docs pages
/// only use the body; blog posts read `title` / `description` / `date`
/// off the map.
fn parse_frontmatter(s: &str) -> (HashMap<String, String>, String) {
    let mut map = HashMap::new();
    let Some(rest) = s.strip_prefix("---\n") else {
        return (map, s.to_string());
    };
    let (block, body) = match rest.find("\n---\n") {
        Some(end) => (&rest[..end], rest[end + 5..].to_string()),
        None => match rest.find("\n---") {
            Some(end) => (&rest[..end], rest[end + 4..].to_string()),
            None => return (map, s.to_string()),
        },
    };
    for line in block.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        map.insert(k.trim().to_string(), v.to_string());
    }
    (map, body)
}

/// Render one markdown body through the shared docs pipeline: write the
/// HTML fragment to `static-docs/<key>.html` and append the matching
/// `page_title` / `page_toc` arms under `key`. Shared by the site.toml
/// docs pages and the discovered blog posts.
#[allow(clippy::too_many_arguments)]
fn emit_page(
    hl: &Hl,
    asset_urls: &AssetUrls,
    body: &str,
    page_path: &str,
    key: &str,
    title: &str,
    static_docs: &Path,
    title_arms: &mut String,
    toc_arms: &mut String,
) {
    let (page_html, toc) = render(hl, body, page_path);
    let page_html = asset_urls.rewrite(&page_html);
    let frag = static_docs.join(format!("{key}.html"));
    if let Some(parent) = frag.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&frag, &page_html).ok();
    title_arms.push_str(&format!("        {:?} => {:?},\n", key, title));
    let mut toc_lit = String::from("&[");
    for (depth, text, id) in &toc {
        toc_lit.push_str(&format!(
            "TocItem {{ depth: {}, text: {:?}, id: {:?} }}, ",
            depth, text, id
        ));
    }
    toc_lit.push(']');
    toc_arms.push_str(&format!("        {:?} => {},\n", key, toc_lit));
}

/// `2026-06-11` → `June 11, 2026`. Falls back to the raw string when
/// the front-matter date isn't ISO `YYYY-MM-DD`.
fn human_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let mut it = iso.split('-');
    let (Some(y), Some(m), Some(d)) = (it.next(), it.next(), it.next()) else {
        return iso.to_string();
    };
    let (Ok(m), Ok(d)) = (m.parse::<usize>(), d.parse::<u32>()) else {
        return iso.to_string();
    };
    if !(1..=12).contains(&m) {
        return iso.to_string();
    }
    format!("{} {}, {}", MONTHS[m - 1], d, y)
}

/// Render markdown → HTML, assigning slug ids to h2/h3 (for anchors)
/// and collecting a table of contents. Internal `*.md` links are
/// rewritten to `/docs/<slug>` routes (or a GitHub blob URL when they
/// escape the docs tree).
fn render(hl: &Hl, md: &str, page_path: &str) -> (String, Vec<(u8, String, String)>) {
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

    // Replace each fenced code block with a syntect-highlighted
    // `<pre><code>…</code></pre>` (one raw-HTML event), so the docs
    // ship pre-coloured markup and the wasm carries no highlighter.
    let mut out_events: Vec<Event> = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::CodeBlock(kind)) = &events[i] {
            let lang = match kind {
                CodeBlockKind::Fenced(info) => {
                    info.split([' ', ',']).next().unwrap_or("").to_string()
                }
                CodeBlockKind::Indented => String::new(),
            };
            let mut src = String::new();
            i += 1;
            while i < events.len() && !matches!(events[i], Event::End(TagEnd::CodeBlock)) {
                if let Event::Text(t) = &events[i] {
                    src.push_str(t);
                }
                i += 1;
            }
            i += 1; // skip End(CodeBlock)
            let body = src.trim_end_matches('\n');
            if lang.eq_ignore_ascii_case("mermaid") {
                // Hand the raw diagram source to the client-side mermaid.js
                // (see index.html). Escape it so `<br/>`, `-->`, `&` survive as
                // text — mermaid reads `textContent`, which the browser
                // un-escapes back to the literal source.
                out_events.push(Event::Html(CowStr::from(format!(
                    "<pre class=\"mermaid\">{}</pre>",
                    esc(body)
                ))));
            } else {
                let inner = hl.code(body, &lang);
                out_events.push(Event::Html(CowStr::from(format!(
                    "<pre><code>{inner}</code></pre>"
                ))));
            }
        } else {
            out_events.push(events[i].clone());
            i += 1;
        }
    }

    let mut out = String::new();
    html::push_html(&mut out, out_events.into_iter());
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

// ── Generated API reference (the `<!-- pp:reactive-api -->` marker) ──────────
//
// Reactive-core modules surfaced in the auto-generated API reference, each
// paired with a friendly section title. Order = reading order on the page.
const REACTIVE_MODULES: &[(&str, &str)] = &[
    ("reactive", "Effects, batching & scheduling"),
    ("computed", "Computed values"),
    ("watch", "Watchers"),
    ("handle", "Handles & this()"),
    ("store", "Stores"),
    ("context", "Context — provide / inject"),
    ("tick", "Microtask scheduling"),
    ("timers", "Timers"),
    ("refs", "Element refs"),
    ("task", "Async tasks"),
    ("emit", "Events"),
    ("signal", "Signals (advanced escape-hatch)"),
];

/// Replace generator markers in a doc body before markdown rendering. The
/// `<!-- pp:reactive-api -->` marker is filled from the live `pocopine-core`
/// source so the API reference can never drift from the code.
fn inject_generated(body: &str, manifest: &str) -> String {
    const MARKER: &str = "<!-- pp:reactive-api -->";
    if !body.contains(MARKER) {
        return body.to_string();
    }
    body.replace(MARKER, &reactive_api_md(manifest))
}

/// Walk the reactive-core modules and emit a markdown API reference: one
/// `##` section per module, every public fn (signature + doc) and public
/// type (doc + public methods). Parsed with `syn` at build time; items are
/// `####` so the page TOC stays at module granularity.
fn reactive_api_md(manifest: &str) -> String {
    let core = Path::new(manifest).join("../../crates/pocopine-core/src");
    println!("cargo:rerun-if-changed={}", core.display());
    let mut out = String::new();
    for (module, title) in REACTIVE_MODULES {
        let path = core.join(format!("{module}.rs"));
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&src) else {
            continue;
        };

        // Public inherent methods, keyed by their self-type name.
        let mut methods: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for item in &file.items {
            let syn::Item::Impl(im) = item else { continue };
            if im.trait_.is_some() {
                continue; // skip trait-impl methods (noise)
            }
            let Some(self_name) = type_path_name(&im.self_ty) else {
                continue;
            };
            for it in &im.items {
                let syn::ImplItem::Fn(m) = it else { continue };
                if !is_pub(&m.vis) || is_doc_hidden(&m.attrs) {
                    continue;
                }
                methods.entry(self_name.clone()).or_default().push((
                    format!("pub {}", clean_code(&m.sig.to_token_stream().to_string())),
                    doc_of(&m.attrs),
                ));
            }
        }

        let mut fns = String::new();
        let mut types = String::new();
        for item in &file.items {
            match item {
                syn::Item::Fn(f) if is_pub(&f.vis) && !is_doc_hidden(&f.attrs) => {
                    let name = f.sig.ident.to_string();
                    let sig = clean_code(&f.sig.to_token_stream().to_string());
                    fns.push_str(&format!(
                        "#### `{name}`\n\n```rust\npub {sig}\n```\n\n{}\n\n",
                        doc_or(&f.attrs)
                    ));
                }
                syn::Item::Struct(s) if is_pub(&s.vis) && !is_doc_hidden(&s.attrs) => {
                    emit_type(
                        &mut types,
                        &s.ident.to_string(),
                        "struct",
                        &doc_or(&s.attrs),
                        &methods,
                    );
                }
                syn::Item::Enum(e) if is_pub(&e.vis) && !is_doc_hidden(&e.attrs) => {
                    emit_type(
                        &mut types,
                        &e.ident.to_string(),
                        "enum",
                        &doc_or(&e.attrs),
                        &methods,
                    );
                }
                syn::Item::Trait(t) if is_pub(&t.vis) && !is_doc_hidden(&t.attrs) => {
                    emit_type(
                        &mut types,
                        &t.ident.to_string(),
                        "trait",
                        &doc_or(&t.attrs),
                        &methods,
                    );
                }
                _ => {}
            }
        }

        if fns.is_empty() && types.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "## {title}\n\n*Module `pocopine_core::{module}`*\n\n"
        ));
        if !fns.is_empty() {
            out.push_str("**Functions**\n\n");
            out.push_str(&fns);
        }
        if !types.is_empty() {
            out.push_str("**Types**\n\n");
            out.push_str(&types);
        }
    }
    out
}

fn emit_type(
    out: &mut String,
    name: &str,
    kind: &str,
    doc: &str,
    methods: &HashMap<String, Vec<(String, String)>>,
) {
    out.push_str(&format!("#### `{name}` ({kind})\n\n{doc}\n\n"));
    if let Some(ms) = methods.get(name)
        && !ms.is_empty()
    {
        out.push_str("Methods:\n\n");
        for (sig, mdoc) in ms {
            if mdoc.is_empty() {
                out.push_str(&format!("- `{sig}`\n"));
            } else {
                out.push_str(&format!("- `{sig}` — {mdoc}\n"));
            }
        }
        out.push('\n');
    }
}

fn doc_or(attrs: &[syn::Attribute]) -> String {
    let d = doc_of(attrs);
    if d.is_empty() {
        "_(no doc comment)_".to_string()
    } else {
        d
    }
}

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// True only for an explicit `#[doc(hidden)]` (a `doc(...)` list), not for
/// a regular `#[doc = "…"]` comment that happens to contain the word.
fn is_doc_hidden(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("doc")
            && matches!(&a.meta, syn::Meta::List(l) if l.tokens.to_string().contains("hidden"))
    })
}

fn type_path_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Collapse the spacing `to_token_stream()` leaves around punctuation so a
/// reconstructed signature reads like source. Cosmetic only.
fn clean_code(s: &str) -> String {
    let mut r = s.to_string();
    for (a, b) in [
        (" :", ":"),
        ("> >", ">>"),
        (" ,", ","),
        (" ;", ";"),
        (" (", "("),
        ("( ", "("),
        (" )", ")"),
        (" <", "<"),
        ("< ", "<"),
        (" >", ">"),
        (" ::", "::"),
        (":: ", "::"),
        ("& ", "&"),
    ] {
        r = r.replace(a, b);
    }
    while r.contains("  ") {
        r = r.replace("  ", " ");
    }
    r.trim().to_string()
}
