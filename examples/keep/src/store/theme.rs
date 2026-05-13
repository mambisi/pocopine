use serde::{Deserialize, Serialize};

const THEME_STORAGE_KEY: &str = "pocopine.keep.theme";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeepTheme {
    #[default]
    Light,
    Dark,
}

impl KeepTheme {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

pub(crate) fn load_theme_preference() -> KeepTheme {
    pocopine::storage::LocalStorage::<KeepTheme>::new(THEME_STORAGE_KEY)
        .get()
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub(crate) fn save_theme_preference(theme: KeepTheme) {
    let _ = pocopine::storage::LocalStorage::new(THEME_STORAGE_KEY).set(&theme);
}
