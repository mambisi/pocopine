//! Combined Form + Fieldset + Field + Input demo. Shows the four
//! form primitives cooperating the way Base UI composes them:
//! Form wraps the submit cycle, Fieldset groups related controls,
//! each Field wires label / input / description / error, and
//! Input drops into the Control slot for the state tracking.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "SignupDemo.poco", style = "signup.css", role = "panel")]
pub struct SignupDemo {
    pub email: String,
    pub password: String,
    pub confirm: String,
    pub errors: HashMap<String, String>,
    pub submitted: bool,
}

#[handlers]
impl SignupDemo {
    /// Fake "server" validation. Returns a map of field-name →
    /// error-message and writes it onto Form.errors so each Field
    /// with a matching `name` auto-flips `invalid=true` via
    /// Pine's inject/watch wiring.
    pub fn submit(&mut self) {
        self.submitted = false;
        let mut errs = HashMap::new();
        if !self.email.contains('@') {
            errs.insert("email".to_string(), "Please enter a valid email.".to_string());
        }
        if self.password.len() < 8 {
            errs.insert(
                "password".to_string(),
                "Use at least 8 characters.".to_string(),
            );
        }
        if self.password != self.confirm {
            errs.insert(
                "confirm".to_string(),
                "Passwords do not match.".to_string(),
            );
        }
        self.errors = errs;
        if self.errors.is_empty() {
            self.submitted = true;
        }
    }
}
