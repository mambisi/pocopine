//! `StorageBrowserStore` — shared client state for the object-store
//! explorer example.

use std::collections::BTreeMap;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    StorageBreadcrumb, StorageBrowserConfigEdit, StorageConnectionSummary, StorageEntry,
    StorageListing, StorageObjectDetail,
};

const DIRECT_UPLOAD_LIMIT_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[store(name = "storage")]
pub struct StorageBrowserStore {
    pub connections: Vec<StorageConnectionSummary>,
    pub connections_empty: bool,
    pub connection_count_label: String,
    pub selected_connection_id: String,
    pub selected_connection_name: String,
    pub selected_provider_label: String,
    pub selected_connection_favicon_url: String,
    pub selected_bucket: String,
    pub selected_endpoint_url: String,
    pub selected_root_prefix: String,
    pub selected_region: String,
    pub selected_access_key_hint: String,
    pub selected_connection_route: String,

    pub entries: Vec<StorageEntry>,
    pub visible_entries: Vec<StorageEntry>,
    pub loaded_listing_connection_id: String,
    pub loaded_listing_prefix: String,
    pub breadcrumbs: Vec<StorageBreadcrumb>,
    pub current_prefix: String,
    pub parent_prefix: String,
    pub path_label: String,
    pub path_title: String,
    pub path_meta: String,
    pub entry_count_label: String,
    pub visible_entry_count_label: String,
    pub entries_empty: bool,
    pub visible_entries_empty: bool,
    pub listing_truncated: bool,
    pub listed_size_label: String,
    pub listed_objects_label: String,
    pub listed_folders_label: String,
    pub current_prefix_label: String,
    pub root_prefix_label: String,
    pub upload_cap_bytes: u64,
    pub upload_cap_label: String,
    pub upload_chunk_bytes: u64,
    pub upload_chunk_label: String,

    pub route_connection_id: String,
    pub route_prefix: String,
    pub listing_request_id: u64,

    pub loading_connections: bool,
    pub loading_entries: bool,
    pub error: String,
    pub modal_connection_id: String,
    pub status: String,

    pub entry_view: String,
    pub search_query: String,
    pub pending_search_query: String,

    pub modal_open: bool,
    pub config_modal_open: bool,

    pub upload_dock_open: bool,
    pub upload_dock_expanded: bool,
    pub upload_metadata: BTreeMap<String, String>,

    pub new_folder_open: bool,

    pub detail_open: bool,
    pub detail_loading: bool,
    pub detail_error: String,
    pub detail: StorageObjectDetail,
    pub detail_request_id: u64,
    /// Whether a previous / next object exists in the current listing,
    /// for the detail panel + lightbox prev/next controls.
    pub detail_has_prev: bool,
    pub detail_has_next: bool,
}

impl Default for StorageBrowserStore {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            connections_empty: true,
            connection_count_label: "0 connections".to_string(),
            selected_connection_id: String::new(),
            selected_connection_name: String::new(),
            selected_provider_label: String::new(),
            selected_connection_favicon_url: String::new(),
            selected_bucket: String::new(),
            selected_endpoint_url: String::new(),
            selected_root_prefix: String::new(),
            selected_region: String::new(),
            selected_access_key_hint: String::new(),
            selected_connection_route: "/".to_string(),

            entries: Vec::new(),
            visible_entries: Vec::new(),
            loaded_listing_connection_id: String::new(),
            loaded_listing_prefix: String::new(),
            breadcrumbs: root_breadcrumbs(),
            current_prefix: String::new(),
            parent_prefix: String::new(),
            path_label: "/".to_string(),
            path_title: "Cloud File Explorer".to_string(),
            path_meta: "No connection selected".to_string(),
            entry_count_label: "0 items".to_string(),
            visible_entry_count_label: "0 items".to_string(),
            entries_empty: true,
            visible_entries_empty: true,
            listing_truncated: false,
            listed_size_label: "0 B listed".to_string(),
            listed_objects_label: "0 objects".to_string(),
            listed_folders_label: "0 folders".to_string(),
            current_prefix_label: "/".to_string(),
            root_prefix_label: "/".to_string(),
            upload_cap_bytes: DIRECT_UPLOAD_LIMIT_BYTES,
            upload_cap_label: crate::format_size(DIRECT_UPLOAD_LIMIT_BYTES),
            upload_chunk_bytes: 1024 * 1024,
            upload_chunk_label: crate::format_size(1024 * 1024),

            route_connection_id: String::new(),
            route_prefix: String::new(),
            listing_request_id: 0,

            loading_connections: false,
            loading_entries: false,
            error: String::new(),
            modal_connection_id: String::new(),
            status: String::new(),

            entry_view: "all".to_string(),
            search_query: String::new(),
            pending_search_query: String::new(),

            modal_open: false,
            config_modal_open: false,

            upload_dock_open: false,
            upload_dock_expanded: false,
            upload_metadata: BTreeMap::new(),

            new_folder_open: false,

            detail_open: false,
            detail_loading: false,
            detail_error: String::new(),
            detail: StorageObjectDetail::default(),
            detail_request_id: 0,
            detail_has_prev: false,
            detail_has_next: false,
        }
    }
}

#[handlers]
impl StorageBrowserStore {
    pub fn sync_route(&mut self, connection_id: String, prefix: String) {
        let prefix = normalize_prefix_value(&prefix);
        if !connection_id.is_empty()
            && self.route_connection_id == connection_id
            && self.route_prefix == prefix
            && self.selected_connection_id == connection_id
            && self.current_prefix == prefix
        {
            return;
        }
        self.apply_route(connection_id, prefix);
    }

    fn apply_route(&mut self, connection_id: String, prefix: String) {
        self.route_connection_id = connection_id;
        self.route_prefix = prefix;
        if self.connections_empty && !self.loading_connections {
            self.load_connections();
            return;
        }
        self.apply_route_selection();
    }

    pub fn load_connections(&mut self) {
        self.loading_connections = true;
        self.error.clear();
        dispatch!(crate::list_storage_connections().await, |s, result| {
            s.loading_connections = false;
            match result {
                Ok(connections) => {
                    s.apply_connections(connections);
                    s.apply_route_selection();
                }
                Err(err) => s.error = err.to_string(),
            }
        },);
    }

    pub fn load_app_config(&mut self) {
        dispatch!(crate::get_storage_browser_config().await, |s, result| {
            match result {
                Ok(config) => s.apply_config_result(config),
                Err(err) => s.error = err.to_string(),
            }
        },);
    }

    pub fn refresh(&mut self) {
        if self.selected_connection_id.is_empty() {
            self.load_connections();
        } else {
            self.browse_prefix(self.current_prefix.clone());
        }
    }

    pub fn select_connection(&mut self, connection_id: String) {
        self.navigate_to(connection_id, String::new());
    }

    pub fn reconnect_connection(&mut self, connection_id: String) {
        let prefix = if self.selected_connection_id == connection_id {
            self.current_prefix.clone()
        } else {
            String::new()
        };
        self.navigate_to(connection_id, prefix);
    }

    pub fn edit_connection(&mut self, connection_id: String) {
        self.modal_connection_id = connection_id;
        self.modal_open = true;
    }

    pub fn browse_prefix(&mut self, prefix: String) {
        if self.selected_connection_id.is_empty() {
            self.error = "add or select a storage connection first".to_string();
            return;
        }
        let connection_id = self.selected_connection_id.clone();
        let prefix = normalize_prefix_value(&prefix);
        self.listing_request_id = self.listing_request_id.wrapping_add(1);
        let request_id = self.listing_request_id;
        self.prepare_pending_listing(&connection_id, &prefix);
        self.error.clear();
        dispatch!(
            crate::browse_storage_connection(connection_id.clone(), prefix.clone()).await,
            |s, result| {
                if !s.is_current_listing_request(request_id, &connection_id, &prefix) {
                    return;
                }
                s.loading_entries = false;
                match result {
                    Ok(listing) => {
                        if listing.connection_id == connection_id
                            && normalize_prefix_value(&listing.prefix) == prefix
                        {
                            s.apply_listing(listing);
                        }
                    }
                    Err(err) => s.error = err.to_string(),
                }
            },
        );
    }

    pub fn open_prefix(&mut self, prefix: String) {
        self.search_query.clear();
        let connection_id = self.selected_connection_id.clone();
        if connection_id.is_empty() {
            self.browse_prefix(prefix);
        } else {
            self.navigate_to(connection_id, prefix);
        }
    }

    pub fn go_up(&mut self) {
        let parent = self.parent_prefix.clone();
        self.open_prefix(parent);
    }

    pub fn set_entry_view(&mut self, view: String) {
        let view = normalize_entry_view(&view);
        if self.entry_view == view {
            return;
        }
        self.entry_view = view;
        self.rebuild_visible_entries();
    }

    pub fn set_search_query(&mut self, query: String) {
        if self.search_query == query {
            return;
        }
        self.search_query = query;
        self.rebuild_visible_entries();
    }

    pub fn clear_search(&mut self) {
        if self.search_query.is_empty() {
            return;
        }
        self.search_query.clear();
        self.rebuild_visible_entries();
    }

    pub fn open_search_result(
        &mut self,
        connection_id: String,
        prefix: String,
        object_name: String,
    ) {
        if connection_id.is_empty() {
            return;
        }
        let prefix = normalize_prefix_value(&prefix);
        self.entry_view = "objects".to_string();
        if self.selected_connection_id == connection_id && self.current_prefix == prefix {
            self.search_query = object_name;
            self.pending_search_query.clear();
            self.rebuild_visible_entries();
        } else {
            self.pending_search_query = object_name;
            self.navigate_to(connection_id, prefix);
        }
    }

    pub fn open_connection_modal(&mut self) {
        self.modal_connection_id.clear();
        self.modal_open = true;
    }

    pub fn close_connection_modal(&mut self) {
        self.modal_open = false;
        self.modal_connection_id.clear();
    }

    pub fn open_config_dialog(&mut self) {
        self.config_modal_open = true;
    }

    pub fn close_config_dialog(&mut self) {
        self.config_modal_open = false;
    }

    pub fn close_detail(&mut self) {
        self.detail_open = false;
        self.detail_loading = false;
        self.detail_error.clear();
        // Bump the request id so any in-flight fetch is ignored on return.
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
    }

    pub fn delete_connection(&mut self, connection_id: String) {
        if connection_id.is_empty() {
            return;
        }
        self.loading_connections = true;
        self.error.clear();
        dispatch!(
            crate::delete_storage_connection(connection_id).await,
            |s, result| {
                s.loading_connections = false;
                match result {
                    Ok(connections) => {
                        s.apply_connections(connections);
                        if s.selected_connection_id.is_empty() {
                            s.navigate_to_root();
                        } else {
                            s.navigate_to(s.selected_connection_id.clone(), String::new());
                        }
                    }
                    Err(err) => s.error = err.to_string(),
                }
            },
        );
    }

    pub fn open_upload_dock(&mut self) {
        self.upload_dock_open = true;
        self.upload_dock_expanded = true;
    }

    pub fn expand_upload_dock(&mut self) {
        self.upload_dock_open = true;
        self.upload_dock_expanded = true;
    }

    pub fn collapse_upload_dock(&mut self) {
        if self.upload_dock_open {
            self.upload_dock_expanded = false;
        }
    }

    pub fn close_upload_dock(&mut self) {
        self.upload_dock_open = false;
        self.upload_dock_expanded = false;
    }

    pub fn open_new_folder_dialog(&mut self) {
        if self.selected_connection_id.is_empty() {
            self.error = "select a storage connection before creating a folder".to_string();
            return;
        }
        self.new_folder_open = true;
    }

    pub fn close_new_folder_dialog(&mut self) {
        self.new_folder_open = false;
    }
}

impl StorageBrowserStore {
    /// Open the file detail panel for a clicked object (by key) and
    /// fetch its metadata + preview URL. The row is looked up from the
    /// current listing to seed name/size instantly.
    pub fn open_object_detail(&mut self, key: String) {
        if key.is_empty() {
            return;
        }
        let connection_id = self.selected_connection_id.clone();
        if connection_id.is_empty() {
            return;
        }
        let row = self
            .entries
            .iter()
            .find(|entry| entry.kind == "object" && entry.key == key)
            .cloned();
        self.detail_open = true;
        self.detail_loading = true;
        self.detail_error.clear();
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
        let request_id = self.detail_request_id;
        // Seed from the listing row so the panel shows name/size/path
        // instantly while the metadata + presigned URL resolve.
        self.detail = StorageObjectDetail {
            connection_id: connection_id.clone(),
            key: key.clone(),
            name: row
                .as_ref()
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| key.rsplit('/').next().unwrap_or(&key).to_string()),
            size_bytes: row
                .as_ref()
                .map(|entry| entry.size_bytes)
                .unwrap_or_default(),
            size_label: row
                .as_ref()
                .map(|entry| entry.size_label.clone())
                .unwrap_or_default(),
            updated_label: row
                .as_ref()
                .map(|entry| entry.modified_label.clone())
                .unwrap_or_default(),
            ..StorageObjectDetail::default()
        };
        let object_keys = self.object_keys();
        let index = object_keys.iter().position(|candidate| candidate == &key);
        self.detail_has_prev = index.is_some_and(|i| i > 0);
        self.detail_has_next = index.is_some_and(|i| i + 1 < object_keys.len());
        dispatch!(
            crate::get_storage_object_detail(connection_id, key).await,
            |s, result| {
                if s.detail_request_id != request_id {
                    return;
                }
                s.detail_loading = false;
                match result {
                    Ok(detail) => {
                        s.detail = detail;
                        s.detail_error.clear();
                    }
                    Err(err) => s.detail_error = err.to_string(),
                }
            },
        );
    }

    /// Keys of the current listing's objects (folders excluded), in
    /// display order — the sequence the lightbox steps through.
    fn object_keys(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == "object")
            .map(|entry| entry.key.clone())
            .collect()
    }

    pub fn detail_next(&mut self) {
        self.step_detail(1);
    }

    pub fn detail_prev(&mut self) {
        self.step_detail(-1);
    }

    /// Move the inspected object by `delta` within the current listing's
    /// objects, loading the adjacent object's detail. No-op at the ends.
    fn step_detail(&mut self, delta: isize) {
        let keys = self.object_keys();
        let Some(current) = keys.iter().position(|key| key == &self.detail.key) else {
            return;
        };
        let target = current as isize + delta;
        if target < 0 || target as usize >= keys.len() {
            return;
        }
        let key = keys[target as usize].clone();
        self.open_object_detail(key);
    }

    fn apply_connections(&mut self, connections: Vec<StorageConnectionSummary>) {
        self.connections = connections;
        self.connections_empty = self.connections.is_empty();
        self.connection_count_label = count_label(self.connections.len(), "connection");
        if !self.route_connection_id.is_empty()
            && self
                .connections
                .iter()
                .any(|connection| connection.id == self.route_connection_id)
        {
            self.selected_connection_id = self.route_connection_id.clone();
            self.current_prefix = self.route_prefix.clone();
        } else if self.selected_connection_id.is_empty()
            || !self
                .connections
                .iter()
                .any(|connection| connection.id == self.selected_connection_id)
        {
            self.selected_connection_id = self
                .connections
                .first()
                .map(|connection| connection.id.clone())
                .unwrap_or_default();
            self.current_prefix.clear();
        }
        self.apply_selected_connection();
    }

    fn apply_route_selection(&mut self) {
        if self.route_connection_id.is_empty() {
            if self.selected_connection_id.is_empty() && !self.connections_empty {
                self.selected_connection_id = self
                    .connections
                    .first()
                    .map(|connection| connection.id.clone())
                    .unwrap_or_default();
            }
            self.current_prefix.clear();
            self.apply_selected_connection();
            if !self.selected_connection_id.is_empty() {
                self.browse_prefix(String::new());
            }
            return;
        }

        if self
            .connections
            .iter()
            .any(|connection| connection.id == self.route_connection_id)
        {
            self.selected_connection_id = self.route_connection_id.clone();
            self.current_prefix = self.route_prefix.clone();
            self.parent_prefix = parent_prefix_value(&self.current_prefix);
            self.search_query.clear();
            self.apply_selected_connection();
            self.browse_prefix(self.route_prefix.clone());
        } else if !self.connections_empty {
            self.error = "route connection was not found".to_string();
        }
    }

    fn apply_selected_connection(&mut self) {
        let selected = self
            .connections
            .iter()
            .find(|connection| connection.id == self.selected_connection_id);
        if let Some(connection) = selected {
            self.selected_connection_name = connection.name.clone();
            self.selected_provider_label = connection.provider_label.clone();
            self.selected_connection_favicon_url = connection.favicon_url.clone();
            self.selected_bucket = connection.bucket.clone();
            self.selected_endpoint_url = connection.endpoint_url.clone();
            self.selected_root_prefix = connection.root_prefix.clone();
            self.selected_region = connection.region.clone();
            self.selected_access_key_hint = connection.access_key_hint.clone();
            self.selected_connection_route = storage_route(&connection.id, &self.current_prefix);
            self.status = format!("{} · {}", connection.provider_label, connection.bucket);
        } else {
            self.selected_connection_name.clear();
            self.selected_provider_label.clear();
            self.selected_connection_favicon_url.clear();
            self.selected_bucket.clear();
            self.selected_endpoint_url.clear();
            self.selected_root_prefix.clear();
            self.selected_region.clear();
            self.selected_access_key_hint.clear();
            self.selected_connection_route = "/".to_string();
            self.status = "No connection selected".to_string();
            self.loaded_listing_connection_id.clear();
            self.loaded_listing_prefix.clear();
            self.entries.clear();
            self.visible_entries.clear();
        }
        self.rebuild_visible_entries();
    }

    fn prepare_pending_listing(&mut self, connection_id: &str, prefix: &str) {
        let prefix = normalize_prefix_value(prefix);
        let listing_changed = self.loaded_listing_connection_id != connection_id
            || self.loaded_listing_prefix != prefix;

        self.selected_connection_id = connection_id.to_string();
        self.current_prefix = prefix;
        self.parent_prefix = parent_prefix_value(&self.current_prefix);
        self.path_label = path_label_for_prefix(&self.current_prefix);
        self.breadcrumbs = breadcrumbs_for_prefix(&self.current_prefix);
        self.loading_entries = true;

        if listing_changed {
            self.entries.clear();
            self.visible_entries.clear();
            self.loaded_listing_connection_id.clear();
            self.loaded_listing_prefix.clear();
            self.listing_truncated = false;
        }

        self.rebuild_visible_entries();
        self.status = if listing_changed {
            "Loading current prefix".to_string()
        } else {
            "Refreshing current prefix".to_string()
        };
    }

    fn is_current_listing_request(
        &self,
        request_id: u64,
        connection_id: &str,
        prefix: &str,
    ) -> bool {
        self.listing_request_id == request_id
            && self.selected_connection_id == connection_id
            && self.current_prefix == normalize_prefix_value(prefix)
    }

    fn apply_listing(&mut self, listing: StorageListing) {
        let listing_connection_id = listing.connection_id.clone();
        let listing_prefix = normalize_prefix_value(&listing.prefix);
        self.loaded_listing_connection_id = listing_connection_id;
        self.loaded_listing_prefix = listing_prefix.clone();
        self.selected_connection_id = listing.connection_id;
        self.selected_connection_name = listing.connection_name;
        self.selected_provider_label = listing.provider_label;
        self.selected_connection_favicon_url = listing.connection_favicon_url;
        self.selected_bucket = listing.bucket;
        self.current_prefix = listing_prefix;
        self.parent_prefix = listing.parent_prefix;
        self.path_label = listing.path_label;
        self.breadcrumbs = listing.breadcrumbs;
        self.entries = listing.entries;
        if !self.pending_search_query.is_empty() {
            self.search_query = std::mem::take(&mut self.pending_search_query);
            self.entry_view = "objects".to_string();
        }
        self.entries_empty = self.entries.is_empty();
        self.listing_truncated = listing.truncated;
        self.status = if self.listing_truncated {
            "Showing first 1,000 entries".to_string()
        } else {
            "Listing current prefix".to_string()
        };
        self.rebuild_visible_entries();
    }

    pub fn apply_saved_connection_result(
        &mut self,
        connections: Vec<StorageConnectionSummary>,
        requested_id: String,
        selected_name: String,
        selected_bucket: String,
    ) {
        self.modal_open = false;
        self.modal_connection_id.clear();
        self.apply_connections(connections);
        if let Some(saved) = self.connections.iter().find(|connection| {
            (!requested_id.is_empty() && connection.id == requested_id)
                || connection.name == selected_name
                || connection.bucket == selected_bucket
        }) {
            self.selected_connection_id = saved.id.clone();
            self.route_connection_id = saved.id.clone();
        }
        self.apply_selected_connection();
        self.navigate_to(self.selected_connection_id.clone(), String::new());
    }

    pub fn apply_config_result(&mut self, config: StorageBrowserConfigEdit) {
        self.upload_cap_bytes = config.active_upload_max_bytes;
        self.upload_cap_label = config.active_upload_max_label;
        self.upload_chunk_bytes = config.active_preferred_chunk_bytes;
        self.upload_chunk_label = config.active_preferred_chunk_label;
    }

    pub fn apply_created_folder_listing(
        &mut self,
        connection_id: String,
        parent_prefix: String,
        listing: StorageListing,
    ) {
        if self.selected_connection_id == connection_id
            && self.current_prefix == normalize_prefix_value(&parent_prefix)
        {
            self.apply_listing(listing);
        }
    }

    fn navigate_to_root(&mut self) {
        self.clear_detail();
        self.route_connection_id.clear();
        self.route_prefix.clear();
        let _ = pocopine::push("/");
    }

    fn navigate_to(&mut self, connection_id: String, prefix: String) {
        if connection_id.is_empty() {
            self.navigate_to_root();
            return;
        }
        // Switching connections clears the inspected file so the detail
        // panel never shows an object from the previous connection.
        if self.selected_connection_id != connection_id {
            self.clear_detail();
        }
        let prefix = normalize_prefix_value(&prefix);
        let route = storage_route(&connection_id, &prefix);
        self.apply_route(connection_id, prefix);
        pocopine::spawn(async move {
            let _ = pocopine::push(route.as_str());
        });
    }

    /// Reset the inspected file to an empty selection (keeps the panel's
    /// open state — it renders an empty placeholder when no file is set).
    fn clear_detail(&mut self) {
        self.detail = StorageObjectDetail::default();
        self.detail_loading = false;
        self.detail_error.clear();
        self.detail_has_prev = false;
        self.detail_has_next = false;
        // Bump the request id so any in-flight fetch is dropped on return.
        self.detail_request_id = self.detail_request_id.wrapping_add(1);
    }

    fn rebuild_visible_entries(&mut self) {
        self.entry_view = normalize_entry_view(&self.entry_view);
        let query = normalize_query(&self.search_query);
        self.visible_entries = self
            .entries
            .iter()
            .filter(|entry| match self.entry_view.as_str() {
                "folders" => entry.kind == "folder",
                "objects" => entry.kind == "object",
                _ => true,
            })
            .filter(|entry| {
                query.is_empty()
                    || entry.name.to_ascii_lowercase().contains(&query)
                    || entry.key.to_ascii_lowercase().contains(&query)
                    || entry.prefix.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect();

        self.entries_empty = self.entries.is_empty();
        self.visible_entries_empty = self.visible_entries.is_empty();
        self.entry_count_label = entry_count_label(&self.entries);
        self.visible_entry_count_label = entry_count_label(&self.visible_entries);
        let listed_bytes = self
            .entries
            .iter()
            .filter(|entry| entry.kind == "object")
            .map(|entry| entry.size_bytes)
            .sum::<u64>();
        let listed_objects = self
            .entries
            .iter()
            .filter(|entry| entry.kind == "object")
            .count();
        let listed_folders = self
            .entries
            .iter()
            .filter(|entry| entry.kind == "folder")
            .count();
        self.listed_size_label = format!("{} listed", crate::format_size(listed_bytes));
        self.listed_objects_label = count_label(listed_objects, "object");
        self.listed_folders_label = count_label(listed_folders, "folder");
        self.current_prefix_label = if self.current_prefix.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.current_prefix)
        };
        self.root_prefix_label = if self.selected_root_prefix.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.selected_root_prefix)
        };
        self.selected_connection_route =
            storage_route(&self.selected_connection_id, &self.current_prefix);
        self.path_title = if self.selected_connection_id.is_empty() {
            "Cloud File Explorer".to_string()
        } else if self.current_prefix.is_empty() {
            "Bucket root".to_string()
        } else {
            prefix_leaf(&self.current_prefix)
        };
        self.path_meta = if self.selected_connection_id.is_empty() {
            "No connection selected".to_string()
        } else {
            format!(
                "{} · {}{}",
                self.selected_connection_name, self.selected_bucket, self.path_label
            )
        };
        self.rebuild_upload_metadata();
    }

    fn rebuild_upload_metadata(&mut self) {
        self.upload_metadata.clear();
        if self.selected_connection_id.is_empty() {
            return;
        }
        self.upload_metadata.insert(
            "connection_id".to_string(),
            self.selected_connection_id.clone(),
        );
        self.upload_metadata
            .insert("prefix".to_string(), self.current_prefix.clone());
    }
}

fn normalize_entry_view(view: &str) -> String {
    match view {
        "folders" | "objects" => view.to_string(),
        _ => "all".to_string(),
    }
}

fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn normalize_prefix_value(prefix: &str) -> String {
    let mut out = prefix.trim().trim_start_matches('/').replace('\\', "/");
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    if !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
    out
}

fn parent_prefix_value(prefix: &str) -> String {
    let prefix = normalize_prefix_value(prefix);
    let trimmed = prefix.trim_end_matches('/');
    let Some((parent, _leaf)) = trimmed.rsplit_once('/') else {
        return String::new();
    };
    format!("{parent}/")
}

fn path_label_for_prefix(prefix: &str) -> String {
    let prefix = normalize_prefix_value(prefix);
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("/{prefix}")
    }
}

pub fn storage_route(connection_id: &str, prefix: &str) -> String {
    if connection_id.trim().is_empty() {
        return "/".to_string();
    }
    let connection = pocopine::encode_route_path_segment(connection_id.trim());
    let prefix = normalize_prefix_value(prefix);
    if prefix.is_empty() {
        format!("/connection/{connection}")
    } else {
        let prefix_path = prefix
            .trim_end_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
            .map(pocopine::encode_route_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        format!("/connection/{connection}/{prefix_path}")
    }
}

fn count_label(count: usize, noun: &str) -> String {
    match count {
        0 => format!("0 {noun}s"),
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

fn entry_count_label(entries: &[StorageEntry]) -> String {
    let folders = entries
        .iter()
        .filter(|entry| entry.kind == "folder")
        .count();
    let objects = entries
        .iter()
        .filter(|entry| entry.kind == "object")
        .count();
    if folders > 0 && objects > 0 {
        format!(
            "{} · {}",
            count_label(folders, "folder"),
            count_label(objects, "object")
        )
    } else if folders > 0 {
        count_label(folders, "folder")
    } else {
        count_label(objects, "object")
    }
}

fn prefix_leaf(prefix: &str) -> String {
    prefix
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .unwrap_or("Bucket root")
        .to_string()
}

fn root_breadcrumbs() -> Vec<StorageBreadcrumb> {
    vec![StorageBreadcrumb {
        label: "Root".to_string(),
        prefix: String::new(),
    }]
}

fn breadcrumbs_for_prefix(prefix: &str) -> Vec<StorageBreadcrumb> {
    let mut crumbs = root_breadcrumbs();
    let mut accumulated = String::new();
    for part in normalize_prefix_value(prefix)
        .split('/')
        .filter(|part| !part.is_empty())
    {
        accumulated.push_str(part);
        accumulated.push('/');
        crumbs.push(StorageBreadcrumb {
            label: part.to_string(),
            prefix: accumulated.clone(),
        });
    }
    crumbs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, name: &str) -> StorageEntry {
        StorageEntry {
            id: format!("{kind}:{name}"),
            kind: kind.to_string(),
            name: name.to_string(),
            key: if kind == "object" {
                name.to_string()
            } else {
                String::new()
            },
            prefix: if kind == "folder" {
                format!("{name}/")
            } else {
                String::new()
            },
            size_bytes: 0,
            size_label: String::new(),
            modified_label: String::new(),
            icon: kind.to_string(),
        }
    }

    #[test]
    fn entry_filters_compose_with_search() {
        let mut store = StorageBrowserStore::default();
        store.entries = vec![
            entry("folder", "logs"),
            entry("object", "logs-2026.json"),
            entry("object", "readme.md"),
        ];

        store.set_entry_view("objects".to_string());
        store.set_search_query("logs".to_string());

        assert_eq!(store.visible_entries.len(), 1);
        assert_eq!(store.visible_entries[0].name, "logs-2026.json");
        assert_eq!(store.visible_entry_count_label, "1 object");
    }

    #[test]
    fn storage_route_encodes_prefix_as_path_segments() {
        assert_eq!(storage_route("abc", ""), "/connection/abc");
        assert_eq!(
            storage_route("abc", "team/docs"),
            "/connection/abc/team/docs"
        );
        assert_eq!(
            storage_route("abc", "team docs/reports"),
            "/connection/abc/team%20docs/reports"
        );
    }

    #[test]
    fn upload_metadata_tracks_selected_route() {
        let mut store = StorageBrowserStore::default();
        store.selected_connection_id = "conn-1".to_string();
        store.current_prefix = "reports/".to_string();

        store.rebuild_visible_entries();

        assert_eq!(
            store
                .upload_metadata
                .get("connection_id")
                .map(String::as_str),
            Some("conn-1")
        );
        assert_eq!(
            store.upload_metadata.get("prefix").map(String::as_str),
            Some("reports/")
        );
    }

    #[test]
    fn pending_listing_clears_stale_entries_and_updates_path() {
        let mut store = StorageBrowserStore::default();
        store.selected_connection_id = "conn-1".to_string();
        store.selected_connection_name = "Remote".to_string();
        store.selected_bucket = "bucket".to_string();
        store.current_prefix = "old/".to_string();
        store.path_label = "/old/".to_string();
        store.loaded_listing_connection_id = "conn-1".to_string();
        store.loaded_listing_prefix = "old/".to_string();
        store.entries = vec![entry("object", "old.txt")];
        store.rebuild_visible_entries();

        store.prepare_pending_listing("conn-1", "videos/recent");

        assert!(store.loading_entries);
        assert!(store.entries.is_empty());
        assert!(store.visible_entries.is_empty());
        assert_eq!(store.current_prefix, "videos/recent/");
        assert_eq!(store.parent_prefix, "videos/");
        assert_eq!(store.path_label, "/videos/recent/");
        assert_eq!(store.path_title, "recent");
        assert_eq!(store.breadcrumbs.len(), 3);
        assert_eq!(store.breadcrumbs[1].label, "videos");
        assert_eq!(store.breadcrumbs[2].label, "recent");
        assert_eq!(
            store.upload_metadata.get("prefix").map(String::as_str),
            Some("videos/recent/")
        );
    }

    #[test]
    fn current_listing_request_rejects_stale_responses() {
        let mut store = StorageBrowserStore::default();
        store.selected_connection_id = "conn-1".to_string();
        store.current_prefix = "videos/".to_string();
        store.listing_request_id = 9;

        assert!(store.is_current_listing_request(9, "conn-1", "videos"));
        assert!(!store.is_current_listing_request(8, "conn-1", "videos"));
        assert!(!store.is_current_listing_request(9, "conn-2", "videos"));
        assert!(!store.is_current_listing_request(9, "conn-1", "archive"));
    }
}
