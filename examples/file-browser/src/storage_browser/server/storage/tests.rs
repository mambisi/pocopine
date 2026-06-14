use crate::storage_browser::GcsConnectionInput;
use crate::storage_browser::server::storage::*;

#[test]
fn normalize_prefix_accepts_root_and_nested_paths() {
    assert_eq!(normalize_prefix(""), "");
    assert_eq!(normalize_prefix("/team/docs"), "team/docs/");
    assert_eq!(normalize_prefix("./team//docs/"), "team/docs/");
}

#[test]
fn breadcrumbs_build_virtual_folder_chain() {
    let crumbs = breadcrumbs("team/docs/reports/");
    let labels = crumbs
        .iter()
        .map(|crumb| crumb.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["Root", "team", "docs", "reports"]);
    assert_eq!(crumbs[2].prefix, "team/docs/");
}

#[test]
fn root_prefix_is_stripped_from_listed_keys() {
    assert_eq!(
        strip_root_prefix("tenant-a/docs/file.txt", "tenant-a"),
        "docs/file.txt"
    );
    assert_eq!(
        strip_root_prefix("docs/file.txt", "tenant-a"),
        "docs/file.txt"
    );
}

#[test]
fn internal_storage_keys_are_hidden_from_browser_listing() {
    assert!(is_internal_storage_key("__pocopine/storage/sessions/x"));
    assert!(is_internal_storage_key("__pocopine/"));
    assert!(is_internal_storage_key(".pocopine-storage/session"));
    assert!(!is_internal_storage_key("docs/__pocopine/file.txt"));
}

#[test]
fn upload_names_are_sanitized_to_file_leaves() {
    assert_eq!(sanitize_upload_name("../plan.pdf"), "plan.pdf");
}

#[test]
fn s3_connection_icon_uses_bucket_for_compatible_endpoints() {
    assert_eq!(s3_connection_icon(""), "brand-aws");
    assert_eq!(
        s3_connection_icon("https://s3.us-east-1.amazonaws.com"),
        "brand-aws"
    );
    assert_eq!(
        s3_connection_icon("https://s3.us-west-004.backblazeb2.com"),
        "bucket"
    );
    assert_eq!(s3_connection_icon("http://127.0.0.1:9000"), "bucket");
}

#[test]
fn connection_favicon_domain_uses_provider_or_endpoint_brand() {
    assert_eq!(
        connection_favicon_domain("s3", "").as_deref(),
        Some("aws.amazon.com")
    );
    assert_eq!(
        connection_favicon_domain("s3", "https://s3.us-west-004.backblazeb2.com").as_deref(),
        Some("backblaze.com")
    );
    assert_eq!(
        connection_favicon_domain("gcs", "").as_deref(),
        Some("cloud.google.com")
    );
    assert_eq!(
        connection_favicon_domain("s3", "http://127.0.0.1:9000").as_deref(),
        None
    );
}

#[test]
fn storage_browser_settings_validate_upload_policy_bounds() {
    assert!(
        StorageBrowserSettings {
            upload_max_bytes: 25 * MIB,
            preferred_chunk_bytes: MIB,
        }
        .validate()
        .is_ok()
    );

    assert!(
        StorageBrowserSettings {
            upload_max_bytes: MIB,
            preferred_chunk_bytes: 2 * MIB,
        }
        .validate()
        .is_err()
    );
}

#[test]
fn storage_browser_config_edit_uses_saved_settings_as_active() {
    let settings = StorageBrowserSettings {
        upload_max_bytes: 70 * MIB,
        preferred_chunk_bytes: 4 * MIB,
    };

    let edit = settings.edit(&active_settings(&settings));

    assert_eq!(edit.active_upload_max_bytes, 70 * MIB);
    assert_eq!(edit.active_preferred_chunk_bytes, 4 * MIB);
    assert!(!edit.restart_required);
}

#[test]
fn upload_policy_for_settings_uses_current_upload_cap() {
    let settings = StorageBrowserSettings {
        upload_max_bytes: 70 * MIB,
        preferred_chunk_bytes: 4 * MIB,
    };

    let policy = upload_policy_for_settings(&settings).unwrap();

    assert_eq!(policy.max_bytes, 70 * MIB);
    assert_eq!(policy.preferred_chunk_size, Some(4 * MIB));
}

#[test]
fn gcs_modified_label_preserves_rfc3339_text() {
    assert_eq!(
        gcs_modified_label(Some("2026-05-29T04:06:03.207Z")),
        "2026-05-29T04:06:03.207Z"
    );
    assert_eq!(gcs_modified_label(None), "");
}

#[test]
fn gcs_legacy_anonymous_flag_maps_to_auth_mode() {
    let connection = SavedGcsConnection::from_input(
        GcsConnectionInput {
            bucket: "demo".to_string(),
            use_anonymous_auth: true,
            ..GcsConnectionInput::default()
        },
        None,
    )
    .unwrap();

    let summary = connection.summary();
    assert_eq!(summary.gcs_auth_mode, "anonymous");
    assert!(summary.use_anonymous_auth);
    assert_eq!(summary.access_key_hint, "anonymous");
}

#[test]
fn gcs_service_account_json_sets_safe_summary_hint() {
    let connection = SavedGcsConnection::from_input(
        GcsConnectionInput {
            bucket: "demo".to_string(),
            auth_mode: "service_account_json".to_string(),
            service_account_json: service_account_json(),
            ..GcsConnectionInput::default()
        },
        None,
    )
    .unwrap();

    let summary = connection.summary();
    assert_eq!(summary.gcs_auth_mode, "service_account_json");
    assert_eq!(
        summary.access_key_hint,
        "browser@example.iam.gserviceaccount.com"
    );
    assert_eq!(summary.project_id, "example-project");
    assert!(summary.gcs_has_service_account_json);
    assert!(!summary.use_anonymous_auth);
}

#[test]
fn gcs_service_account_edit_preserves_saved_json_when_blank() {
    let first = SavedGcsConnection::from_input(
        GcsConnectionInput {
            bucket: "demo".to_string(),
            auth_mode: "service_account_json".to_string(),
            service_account_json: service_account_json(),
            ..GcsConnectionInput::default()
        },
        None,
    )
    .unwrap();

    let updated = SavedGcsConnection::from_input(
        GcsConnectionInput {
            id: first.id.clone(),
            bucket: "demo".to_string(),
            auth_mode: "service_account_json".to_string(),
            service_account_json: String::new(),
            ..GcsConnectionInput::default()
        },
        Some(first.effective_auth()),
    )
    .unwrap();

    assert_eq!(
        updated.summary().access_key_hint,
        "browser@example.iam.gserviceaccount.com"
    );
}

#[test]
fn gcs_service_account_json_is_required_for_new_connection() {
    let result = SavedGcsConnection::from_input(
        GcsConnectionInput {
            bucket: "demo".to_string(),
            auth_mode: "service_account_json".to_string(),
            ..GcsConnectionInput::default()
        },
        None,
    );

    assert!(result.is_err());
}

#[test]
fn folder_names_are_single_safe_segments() {
    assert_eq!(sanitize_folder_name(" Reports ").unwrap(), "Reports");
    assert!(sanitize_folder_name("../Reports").is_err());
    assert!(sanitize_folder_name("__pocopine").is_err());
    assert!(sanitize_folder_name("").is_err());
}

fn service_account_json() -> String {
    serde_json::json!({
        "type": "service_account",
        "project_id": "example-project",
        "private_key_id": "test-private-key-id",
        "private_key": "-----BEGIN PRIVATE KEY-----\nredacted\n-----END PRIVATE KEY-----\n",
        "client_email": "browser@example.iam.gserviceaccount.com"
    })
    .to_string()
}
