//! pocopine — the marketing site.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// ─── shared types ────────────────────────────────────────────────

mod shared {
    use serde::{Deserialize, Serialize};

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
}

use shared::{ContactMessage, ContactResponse};

// ─── server functions ───────────────────────────────────────────

#[pocopine::server(public)]
pub async fn submit_contact(msg: ContactMessage) -> ServerResult<ContactResponse> {
    // Contact fields may contain personal data; keep raw payloads out of logs.
    tracing::info!(
        target: "pocopine.log",
        message_len = msg.message.len(),
        "contact form submitted"
    );
    let id = (msg.message.len() as u32).wrapping_add(1000);
    Ok(ContactResponse { id, ok: true })
}

// ─── components ─────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn on_mount(&mut self) {
        self.count = 0;
    }
    pub fn increment(&mut self) {
        self.count += 1;
    }
    pub fn decrement(&mut self) {
        self.count -= 1;
    }
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct Home {}

#[handlers]
impl Home {}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct Contact {
    pub name: String,
    pub email: String,
    pub message: String,
    pub submitting: bool,
    pub submitted: bool,
    pub last_id: u32,
    pub error: String,
}

#[handlers]
impl Contact {
    pub fn submit(&mut self) {
        if self.name.trim().is_empty()
            || self.email.trim().is_empty()
            || self.message.trim().is_empty()
        {
            self.error = "please fill out every field".into();
            return;
        }
        self.error.clear();
        self.submitting = true;
        let msg = ContactMessage {
            name: self.name.clone(),
            email: self.email.clone(),
            message: self.message.clone(),
        };
        dispatch!(submit_contact(msg).await, |s, result| {
            s.submitting = false;
            match result {
                Ok(resp) => {
                    s.submitted = true;
                    s.last_id = resp.id;
                    s.name.clear();
                    s.email.clear();
                    s.message.clear();
                }
                Err(e) => {
                    s.error = e.to_string();
                }
            }
        },);
    }
}

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .register::<Counter>()
        .route::<Home>("/")
        .route::<Contact>("/contact")
        .route::<NotFound>("*")
        .run();
}
