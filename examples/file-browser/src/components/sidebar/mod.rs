//! `<file-browser-sidebar>` — workspace nav + storage indicator.
//! Reads `total_size_label`, `storage_quota_label`, `storage_percent`
//! from [`crate::FileBrowserStore`].

use pine::{PineProgressIndicator, PineProgressRoot};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserSidebar.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon, PineProgressRoot, PineProgressIndicator]
)]
pub struct FileBrowserSidebar {}

#[handlers]
impl FileBrowserSidebar {}
