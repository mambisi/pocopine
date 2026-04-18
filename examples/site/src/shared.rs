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
}

/// Listing shape — drops `body` for lighter responses.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArticleSummary {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub excerpt: String,
}

impl From<Article> for ArticleSummary {
    fn from(a: Article) -> Self {
        ArticleSummary {
            slug: a.slug,
            title: a.title,
            date: a.date,
            excerpt: a.excerpt,
        }
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
