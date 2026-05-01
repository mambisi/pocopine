//! Top-level shell: header, nav, `<pp-outlet>` for the router, footer.
//! Also owns the "about" dialog state — demo of `pp-teleport` + `pp-if`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "app_shell.css")]
pub struct AppShell {
    pub about_open: bool,
}

#[handlers]
impl AppShell {
    pub fn open_about(&mut self) {
        self.about_open = true;
    }
    pub fn close_about(&mut self) {
        self.about_open = false;
    }
}
