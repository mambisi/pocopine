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

use pulldown_cmark::{
    html, CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
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
            let body = strip_frontmatter(&raw);
            let (page_html, toc) = render(&hl, &body, &p.path);
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

    emit_snippets(&hl, &manifest, &out_dir);
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

    // Component reference Code tabs: every showcase demo's `.poco`
    // source, highlighted as markup. The slug is the demo directory
    // with `_` → `-` (matching `component_meta`), so the table stays in
    // sync with the demos on disk without a duplicated path list.
    let showcase = Path::new(manifest).join("src/components/showcase");
    println!("cargo:rerun-if-changed={}", showcase.display());
    let mut demos: Vec<(String, String)> = Vec::new();
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

    let dest = Path::new(out_dir).join("gen_code.rs");
    fs::write(dest, s).unwrap();
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
        desc: "A component is a struct plus a sibling .poco template. State is just fields; handlers mutate &mut self. No virtual DOM and no Rc<RefCell> — pocopine makes the fields reactive and updates the real DOM in place.",
        doc: "/docs/guides/components/README",
        code: "#[derive(Default, Serialize, Deserialize)]\n#[component(template = \"Todo.poco\")]\npub struct TodoApp {\n    items: Vec<Todo>,\n    draft: String,\n}\n\n#[handlers]\nimpl TodoApp {\n    pub fn add(&mut self) {\n        self.items.push(Todo::new(&self.draft));\n        self.draft.clear();\n    }\n}",
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
        desc: "Mark an async fn #[server] and it compiles to a backend route plus a typed client stub. You call it from the browser as a normal async fn — same language, same types, both ends. No fetch glue, no hand-written endpoints.",
        doc: "/docs/guides/server/server-plugins",
        code: "// one function, two build targets\n#[pocopine::server]\nasync fn close(id: Uuid) -> ServerResult<()> {\n    db().close_issue(id).await?;\n    Ok(())\n}\n\n// the client calls close(id).await like any async fn",
    },
    ShowcaseFeat {
        name: "Data & live",
        file: "issues.rs",
        lang: "rust",
        title: "Local-first data, live-synced",
        desc: "Query data with #[query_resource]: reactive, offline-first views that update optimistically and sync in the background. Live invalidation pushes each change to every connected client automatically.",
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
        desc: "`pocopine deploy` builds an image and deploys the web and worker processes to Railway or Render through their APIs — no host CLIs. Or run the very same container anywhere Docker runs.",
        doc: "/docs/getting-started/introduction",
        code: "$ pocopine build --release\n$ pocopine deploy\n  ✓ web + worker → railway\n  → https://app.up.railway.app",
    },
    ShowcaseFeat {
        name: "Observability",
        file: "checkout.rs",
        lang: "rust",
        title: "Observability, built in",
        desc: "One structured-event contract feeds logging, OpenTelemetry tracing, and analytics sinks — the same API in the browser and on the server, with privacy labels so sensitive fields never leak into logs.",
        doc: "/docs/guides/observability/logging-tracing",
        code: "tracing::info!(\n    target: \"pocopine.log\",\n    user = %id,\n    \"checkout completed\",\n);\n// one event → logging · OTLP tracing · analytics",
    },
];

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
            let inner = hl.code(src.trim_end_matches('\n'), &lang);
            out_events.push(Event::Html(CowStr::from(format!(
                "<pre><code>{inner}</code></pre>"
            ))));
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
