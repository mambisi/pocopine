//! Interactive "issue tracker" preview for the `#full-stack` section.
//! It morphs with the active flow step (`#[prop] stage`): the app card
//! gains a live badge (≥3), a signed-in user (≥4), and at the final
//! step becomes a deployed browser frame with a structured-log stream.
//! A scope-bound interval simulates the server pushing new issues to
//! every client; rows carry an assignee avatar (DiceBear — a free,
//! seed-deterministic avatar API). A hand-built demo of the pitch:
//! components + live data + auth + observability, end to end.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Issue {
    pub id: u32,
    pub title: String,
    /// Freshly arrived (optimistic / live-pushed) → sync pulse.
    pub live: bool,
    pub label: String,
    pub assignee: String,
    /// Pre-built avatar URL (bound to `<img :src>`).
    pub avatar: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct LogLine {
    pub id: u32,
    pub target: String,
    pub msg: String,
}

/// New issues the server pushes to every client `(title, label, who)`.
const SERVER_ISSUES: &[(&str, &str, &str)] = &[
    ("Rate-limit the public API", "infra", "Mateo"),
    ("Upgrade to Pine 0.9", "chore", "Priya"),
    ("Investigate flaky deploy test", "bug", "Sam"),
    ("Add CSV export", "feat", "Lena"),
    ("Cache avatar images", "infra", "Noah"),
    ("Tighten the members_only guard", "bug", "Ava"),
];

/// The signed-in user's avatar seed.
const ME: &str = "Riley Quinn";

/// Build a free, seed-deterministic avatar URL (DiceBear).
fn avatar(seed: &str) -> String {
    format!(
        "https://api.dicebear.com/9.x/notionists/svg?seed={}&backgroundColor=f8eeda",
        seed.replace(' ', "%20")
    )
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "IssueFlowDemo.poco", style = "issue_flow_demo.css")]
pub struct IssueFlowDemo {
    /// Active flow step (1-5), bound from the parent's scroll-spy.
    #[prop]
    pub stage: u32,
    pub issues: Vec<Issue>,
    pub draft: String,
    pub logs: Vec<LogLine>,
    /// The signed-in user's avatar (header chip).
    pub me_avatar: String,
    next_id: u32,
    next_log: u32,
    pushed: u32,
}

#[handlers]
impl IssueFlowDemo {
    pub fn on_setup(&mut self) {
        self.me_avatar = avatar(ME);
        self.issues = vec![
            Issue {
                id: 1,
                title: "Fix login redirect".into(),
                live: false,
                label: "bug".into(),
                assignee: "Ava".into(),
                avatar: avatar("Ava"),
            },
            Issue {
                id: 2,
                title: "Add dark-mode toggle".into(),
                live: false,
                label: "feat".into(),
                assignee: "Mateo".into(),
                avatar: avatar("Mateo"),
            },
        ];
        self.next_id = 3;
        self.next_log = 1;

        // Simulate live sync — the server pushes new issues to every
        // client. The tick no-ops until the "live" step is on screen.
        let handle = this::<Self>();
        pocopine::timers::every_scoped(3200, move || {
            handle.update(|c| c.server_push());
        });
    }

    pub fn add(&mut self) {
        let title = self.draft.trim().to_string();
        if title.is_empty() {
            return;
        }
        self.issues.push(Issue {
            id: self.next_id,
            title,
            live: true,
            label: "feat".into(),
            assignee: "you".into(),
            avatar: avatar(ME),
        });
        self.next_id += 1;
        self.draft.clear();
        // One write → the structured-event contract fans out.
        self.log(
            "pocopine.trace",
            "server_function_completed · create_issue · 12ms",
        );
        self.log("pocopine.log", "issue created · 201 · /api/create_issue");
        self.log("pocopine.analytics", "issue_created · workspace=acme");
    }
}

impl IssueFlowDemo {
    /// A live invalidation arriving from the server — another client or
    /// a worker created an issue, and it shows up here in real time.
    fn server_push(&mut self) {
        // Only stream arrivals once the "live" chapter is on screen.
        if self.stage < 3 {
            return;
        }
        let (title, label, who) = SERVER_ISSUES[self.pushed as usize % SERVER_ISSUES.len()];
        self.pushed += 1;
        self.issues.push(Issue {
            id: self.next_id,
            title: title.into(),
            live: true,
            label: label.into(),
            assignee: who.into(),
            avatar: avatar(who),
        });
        self.next_id += 1;
        // Keep the live feed bounded.
        let n = self.issues.len();
        if n > 7 {
            self.issues.drain(0..n - 7);
        }
        self.log("pocopine.live", "invalidation · issues · upsert");
    }

    /// Append a structured-event line (not a handler — kept off the
    /// `#[handlers]` impl so its `&str` args aren't treated as args).
    fn log(&mut self, target: &str, msg: &str) {
        self.logs.push(LogLine {
            id: self.next_log,
            target: target.into(),
            msg: msg.into(),
        });
        self.next_log += 1;
        let n = self.logs.len();
        if n > 6 {
            self.logs.drain(0..n - 6);
        }
    }
}
