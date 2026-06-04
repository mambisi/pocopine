//! "The whole stack" section — a Laravel-style feature showcase. The
//! left column sells the framework (heading, checklist, docs CTA); the
//! right column is a pill-tabbed, editor-styled code panel. Tabs are
//! resolved from a delegated click via each tab's `data-feat`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

struct Feat {
    /// Tab label — kept for reference; the tabs are authored in the
    /// template, so this isn't read at runtime.
    #[allow(dead_code)]
    name: &'static str,
    file: &'static str,
    lang: &'static str,
    code: &'static str,
}

const FEATS: &[Feat] = &[
    Feat {
        name: "Components",
        file: "Todo.rs",
        lang: "rust",
        code: "#[derive(Default, Serialize, Deserialize)]\n#[component(template = \"Todo.poco\")]\npub struct TodoApp {\n    items: Vec<Todo>,\n    draft: String,\n}\n\n#[handlers]\nimpl TodoApp {\n    pub fn add(&mut self) {\n        self.items.push(Todo::new(&self.draft));\n        self.draft.clear();\n    }\n}",
    },
    Feat {
        name: "Server functions",
        file: "api.rs",
        lang: "rust",
        code: "// one function, two build targets\n#[pocopine::server]\nasync fn close(id: Uuid) -> ServerResult<()> {\n    db().close_issue(id).await?;\n    Ok(())\n}\n\n// the client calls close(id).await like any async fn",
    },
    Feat {
        name: "Data & live",
        file: "issues.rs",
        lang: "rust",
        code: "// local-first, reactive, live-synced\n#[query_resource]\nstruct Issues;\n\nlet open = Issues::query(&client)\n    .filter(|i| i.open)\n    .observe();      // updates push to every client",
    },
    Feat {
        name: "Auth & services",
        file: "main.rs",
        lang: "rust",
        code: "app! {\n    plugins: [\n        Credentials::new(users),        // email + password\n        JwtVerifier::firebase(project),  // Clerk / Auth0 / Supabase\n        Storage::s3(bucket),             // uploads\n        Jobs::redis(url),                // background jobs\n    ],\n}",
    },
    Feat {
        name: "Deploy",
        file: "shell",
        lang: "bash",
        code: "$ pocopine build --release\n$ pocopine deploy\n  ✓ web + worker → railway\n  → https://app.up.railway.app",
    },
    Feat {
        name: "Observability",
        file: "checkout.rs",
        lang: "rust",
        code: "tracing::info!(\n    target: \"pocopine.log\",\n    user = %id,\n    \"checkout completed\",\n);\n// one event → logging · OTLP tracing · analytics",
    },
];

#[derive(Default, Serialize, Deserialize)]
#[component(template = "StackShowcase.poco", style = "stack_showcase.css")]
pub struct StackShowcase {
    pub active: u32,
    pub file: String,
    pub lang: String,
    pub code: String,
}

#[handlers]
impl StackShowcase {
    pub fn on_setup(&mut self) {
        self.apply();
    }

    fn apply(&mut self) {
        let f = &FEATS[self.active as usize];
        self.file = f.file.into();
        self.lang = f.lang.into();
        self.code = f.code.into();
    }

    pub fn on_tab(&mut self, ev: web_sys::MouseEvent) {
        use wasm_bindgen::JsCast;
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if let Ok(Some(btn)) = el.closest("[data-feat]") {
            if let Some(v) = btn.get_attribute("data-feat") {
                if let Ok(n) = v.parse::<u32>() {
                    if (n as usize) < FEATS.len() {
                        self.active = n;
                        self.apply();
                    }
                }
            }
        }
    }
}
