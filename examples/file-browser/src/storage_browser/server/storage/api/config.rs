use pocopine::ServerResult;

use crate::storage_browser::server::storage::*;
use crate::storage_browser::{StorageBrowserConfigEdit, StorageBrowserConfigInput};

pub(crate) fn get_app_config() -> ServerResult<StorageBrowserConfigEdit> {
    let settings = load_config()?.settings.validate()?;
    let active = active_settings(&settings);
    Ok(settings.edit(&active))
}

pub(crate) fn save_app_config(
    input: StorageBrowserConfigInput,
) -> ServerResult<StorageBrowserConfigEdit> {
    let mut config = load_config()?;
    config.settings = StorageBrowserSettings::from_input(input)?;
    save_config(&config)?;
    let active = active_settings(&config.settings);
    Ok(config.settings.edit(&active))
}
