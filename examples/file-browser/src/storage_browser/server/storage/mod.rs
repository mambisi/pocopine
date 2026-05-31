mod api;
mod backend;
mod config;
mod consts;
mod model;
mod providers;
mod utils;

#[cfg(test)]
mod tests;

pub(crate) use api::{
    browse_connection, create_folder, delete_connection, get_app_config, get_connection,
    list_connections, list_object_commands, object_detail, save_app_config, save_gcs_connection,
    save_s3_connection,
};
pub(crate) use backend::storage_server;

// Shared glue: re-export the internal building blocks so the leaf submodules
// (api/*, providers/*, backend, tests) can `use crate::storage_browser::server::storage::*`
// and see consts, config, model, helpers, and provider impls in one place.
pub(crate) use config::*;
pub(crate) use consts::*;
pub(crate) use model::*;
pub(crate) use providers::gcs::*;
pub(crate) use providers::s3::*;
pub(crate) use utils::*;
