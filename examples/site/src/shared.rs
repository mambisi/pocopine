//! Wire types shared by the client and server halves of the site.

use serde::{Deserialize, Serialize};

/// Full article — used by the detail page and stored in articles.json.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Article {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub excerpt: String,
    pub body: String,
    /// Size of `body` in bytes. Computed server-side so the client
    /// doesn't have to re-measure on every fetch.
    #[serde(default)]
    pub bytes: usize,
    /// Human-readable size, e.g. `"12.3 KB"`. Precomputed for display
    /// without pulling a formatter into the wasm bundle.
    #[serde(default)]
    pub size_label: String,
}

/// Listing shape — drops `body` for lighter responses, keeps the size
/// of that body so the list can display it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArticleSummary {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub excerpt: String,
    #[serde(default)]
    pub bytes: usize,
    #[serde(default)]
    pub size_label: String,
}

impl From<Article> for ArticleSummary {
    fn from(a: Article) -> Self {
        ArticleSummary {
            slug: a.slug,
            title: a.title,
            date: a.date,
            excerpt: a.excerpt,
            bytes: a.bytes,
            size_label: a.size_label,
        }
    }
}

/// Turn a byte count into a short, human-friendly label:
/// `512` → `"512 B"`, `4096` → `"4.0 KB"`, `1_500_000` → `"1.4 MB"`.
pub fn human_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n_f = n as f64;
    if n_f < KB {
        format!("{n} B")
    } else if n_f < MB {
        format!("{:.1} KB", n_f / KB)
    } else {
        format!("{:.1} MB", n_f / MB)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContactMessage {
    pub name: String,
    pub email: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContactResponse {
    pub id: u32,
    pub ok: bool,
}
