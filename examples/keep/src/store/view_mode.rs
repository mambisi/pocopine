use serde::{Deserialize, Serialize};

const VIEW_MODE_STORAGE_KEY: &str = "pocopine.keep.view_mode";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepViewMode {
    #[default]
    Masonry,
    List,
}

impl KeepViewMode {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Masonry => Self::List,
            Self::List => Self::Masonry,
        }
    }
}

pub(crate) fn load_view_mode_preference() -> KeepViewMode {
    pocopine::storage::LocalStorage::<KeepViewMode>::new(VIEW_MODE_STORAGE_KEY)
        .get()
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub(crate) fn save_view_mode_preference(mode: KeepViewMode) {
    let _ = pocopine::storage::LocalStorage::new(VIEW_MODE_STORAGE_KEY).set(&mode);
}
