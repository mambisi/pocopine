//! Front-page list + search. Calls the `search_stories` server function
//! and renders each hit through `pp-for` against a `Vec<StoryView>`.

use pocopine::prelude::*;
use pocopine::refs;
use serde::{Deserialize, Serialize};
use web_sys::{HtmlInputElement, InputEvent};

use crate::shared::Story;
use crate::{extract_domain, humanize_age, performance_now, search_stories};

/// Row shape the StoryList template iterates. All display-time
/// derivations live here so the `pp-for` body stays declarative.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct StoryView {
    pub id: String,
    pub title: String,
    pub title_href: String,
    pub external: bool,
    pub domain: String,
    pub points: i32,
    pub author: String,
    pub age: String,
    pub item_url: String,
    pub comments_label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(style = "story_list.css")]
pub struct StoryList {
    pub query: String,
    pub applied_query: String,
    pub loading: bool,
    pub error: String,
    pub stories: Vec<StoryView>,
    pub count: u32,
    pub fetch_ms: u32,
}

#[handlers]
impl StoryList {
    pub fn on_mount(&mut self) {
        // Focus the search input on mount — imperative DOM reach via
        // pp-ref="search" on the <input>.
        if let Some(input) = refs::get_as::<HtmlInputElement>("search") {
            let _ = input.focus();
        }
        if self.count > 0 {
            return;
        }
        self.run_search();
    }

    /// Triggered by the search form (pp-on:submit.prevent) or any
    /// button that wants to re-run the current query.
    pub fn search(&mut self) {
        self.run_search();
    }

    /// Debounced keystroke handler. Pulls the value off the event's
    /// target, stores it, and kicks off a search. Replaces the
    /// older `pp-model="query"` + `pp-on:input.debounce` pair —
    /// single event-driven path now that RFC-008 lets handlers
    /// receive the raw `InputEvent`.
    pub fn on_search_input(&mut self, ev: InputEvent) {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        self.query = input.value();
        self.run_search();
    }

    fn run_search(&mut self) {
        self.loading = true;
        let query = self.query.clone();
        let start = performance_now();
        dispatch!(search_stories(query.clone()).await, |s, result| {
            s.loading = false;
            s.applied_query = query.clone();
            s.fetch_ms = (performance_now() - start).round().max(0.0) as u32;
            match result {
                Ok(stories) => {
                    s.count = stories.len() as u32;
                    s.stories = stories.into_iter().map(story_to_view).collect();
                    s.error.clear();
                }
                Err(e) => s.error = e.to_string(),
            }
        },);
    }
}

fn story_to_view(s: Story) -> StoryView {
    let url = s.url.clone().unwrap_or_default();
    let external = !url.is_empty();
    let domain = extract_domain(&url);
    let item_url = format!("/item/{}", s.id);
    let title_href = if external { url } else { item_url.clone() };
    let comments_label = match s.num_comments {
        None | Some(0) => "discuss".to_string(),
        Some(1) => "1 comment".to_string(),
        Some(n) => format!("{n} comments"),
    };
    StoryView {
        id: s.id,
        title: s.title,
        title_href,
        external,
        domain,
        points: s.points.unwrap_or(0),
        author: s.author,
        age: humanize_age(s.created_at_i),
        item_url,
        comments_label,
    }
}
