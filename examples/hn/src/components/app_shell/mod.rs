//! Top-level shell: header, nav, `<pp-outlet>` for the router, footer.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "app_shell.css")]
pub struct AppShell {}

#[handlers]
impl AppShell {}
