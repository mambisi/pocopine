//! `<keep-grid-layout>` — Google Keep–style masonry layout.
//!
//! Stateless shell: it reads `pinned_notes` / `other_notes` from the
//! shared `KeepStore` and lays them out as a column-masonry grid,
//! with the composer at the top in the Notes section. Owns no
//! durable state of its own; the `KeepBoard` switches between this
//! and `<keep-list-detail>` based on `view_mode`.

use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{composer::KeepComposer, editor::KeepEditor, note_card::KeepNoteCard};

#[derive(Default, Serialize, Deserialize)]
#[component(
    style = "KeepGridLayout.css",
    uses = [PineIcon, KeepComposer, KeepNoteCard, KeepEditor]
)]
pub struct KeepGridLayout {}

#[handlers]
impl KeepGridLayout {}
