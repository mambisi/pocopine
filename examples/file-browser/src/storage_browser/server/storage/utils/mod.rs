use percent_encoding::utf8_percent_encode;
use pocopine::{ServerError, ServerResult};

use crate::storage_browser::StorageBreadcrumb;
use crate::storage_browser::server::storage::*;

/// Classify how an object can be previewed in the browser. Prefers the
/// MIME type, falling back to the file extension when it's absent.
pub(crate) fn preview_kind_for(content_type: &str, key: &str) -> String {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let kind = if mime.starts_with("image/") {
        "image"
    } else if mime == "application/pdf" {
        "pdf"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/xml"
        || mime == "application/javascript"
    {
        "text"
    } else if mime.is_empty() {
        match key
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif") => {
                "image"
            }
            Some("pdf") => "pdf",
            Some("mp4" | "webm" | "mov" | "m4v" | "ogv") => "video",
            Some("mp3" | "wav" | "ogg" | "oga" | "m4a" | "flac") => "audio",
            Some(
                "txt" | "md" | "json" | "xml" | "csv" | "log" | "yaml" | "yml" | "toml" | "rs"
                | "js" | "ts" | "css" | "html",
            ) => "text",
            _ => "none",
        }
    } else {
        "none"
    };
    kind.to_string()
}

pub(crate) fn normalize_prefix(prefix: &str) -> String {
    let mut out = prefix.trim().trim_start_matches('/').replace('\\', "/");
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    while out.starts_with("./") {
        out = out[2..].to_string();
    }
    if out == "." {
        out.clear();
    }
    if !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
    out
}

pub(crate) fn join_prefixes(root: &str, prefix: &str) -> String {
    format!("{}{}", normalize_prefix(root), normalize_prefix(prefix))
}

/// Reconstruct an object's full key from the connection root + the
/// listing-relative key. Unlike [`join_prefixes`], this does NOT append
/// a trailing `/` — objects are not prefixes, and a trailing slash makes
/// `head_object` miss (the "head object: service error").
pub(crate) fn join_object_key(root: &str, relative_key: &str) -> String {
    format!("{}{}", normalize_prefix(root), relative_key)
}

pub(crate) fn sanitize_upload_name(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    let candidate = normalized
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('.');
    candidate
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>()
}

pub(crate) fn sanitize_folder_name(name: &str) -> ServerResult<String> {
    let candidate = name.trim().trim_matches('.').to_string();
    if candidate.is_empty() {
        return Err(ServerError::App("folder name is required".to_string()));
    }
    if candidate.contains('/') || candidate.contains('\\') {
        return Err(ServerError::App(
            "folder name cannot contain path separators".to_string(),
        ));
    }
    if candidate.chars().any(|ch| ch == '\0' || ch.is_control()) {
        return Err(ServerError::App(
            "folder name contains unsupported characters".to_string(),
        ));
    }
    if matches!(candidate.as_str(), "." | "..") || is_internal_storage_key(&candidate) {
        return Err(ServerError::App("folder name is reserved".to_string()));
    }
    Ok(candidate)
}

pub(crate) fn strip_root_prefix<'a>(key: &'a str, root: &str) -> &'a str {
    let root = normalize_prefix(root);
    key.strip_prefix(&root).unwrap_or(key)
}

pub(crate) fn parent_prefix(prefix: &str) -> String {
    let prefix = normalize_prefix(prefix);
    let trimmed = prefix.trim_end_matches('/');
    let Some((parent, _leaf)) = trimmed.rsplit_once('/') else {
        return String::new();
    };
    format!("{parent}/")
}

pub(crate) fn breadcrumbs(prefix: &str) -> Vec<StorageBreadcrumb> {
    let mut crumbs = vec![StorageBreadcrumb {
        label: "Root".to_string(),
        prefix: String::new(),
    }];
    let mut acc = String::new();
    for part in normalize_prefix(prefix)
        .split('/')
        .filter(|part| !part.is_empty())
    {
        acc.push_str(part);
        acc.push('/');
        crumbs.push(StorageBreadcrumb {
            label: part.to_string(),
            prefix: acc.clone(),
        });
    }
    crumbs
}

pub(crate) fn path_label(prefix: &str) -> String {
    let prefix = normalize_prefix(prefix);
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("/{prefix}")
    }
}

pub(crate) fn prefix_leaf(prefix: &str) -> String {
    normalize_prefix(prefix)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("folder")
        .to_string()
}

pub(crate) fn object_leaf(key: &str) -> String {
    key.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(key)
        .to_string()
}

pub(crate) fn is_internal_storage_key(key: &str) -> bool {
    key.split('/')
        .next()
        .is_some_and(|segment| matches!(segment, "__pocopine" | ".pocopine" | ".pocopine-storage"))
}

pub(crate) fn access_key_hint(access_key_id: &str) -> String {
    if access_key_id.len() <= 4 {
        return "****".to_string();
    }
    let suffix = &access_key_id[access_key_id.len() - 4..];
    format!("****{suffix}")
}

pub(crate) fn provider_label(provider: &str) -> &'static str {
    match provider {
        "s3" => "S3",
        "gcs" => "GCS",
        _ => "Storage",
    }
}

pub(crate) fn s3_connection_icon(endpoint_url: &str) -> &'static str {
    let endpoint = endpoint_url.trim().to_ascii_lowercase();
    if endpoint.is_empty() || endpoint.contains("amazonaws.com") {
        "brand-aws"
    } else {
        "bucket"
    }
}

pub(crate) fn connection_favicon_url(provider: &str, endpoint_url: &str) -> String {
    let Some(domain) = connection_favicon_domain(provider, endpoint_url) else {
        return String::new();
    };
    format!(
        "https://www.google.com/s2/favicons?domain={}&sz=32",
        encode_uri_component(&domain)
    )
}

pub(crate) fn connection_favicon_domain(provider: &str, endpoint_url: &str) -> Option<String> {
    let host = endpoint_host(endpoint_url);
    if provider == "gcs" && (host.is_empty() || host.ends_with("googleapis.com")) {
        return Some("cloud.google.com".to_string());
    }
    if provider == "s3" && (host.is_empty() || host.contains("amazonaws.com")) {
        return Some("aws.amazon.com".to_string());
    }
    if host.is_empty() || is_private_or_local_host(&host) {
        return None;
    }
    if host == "backblazeb2.com" || host.ends_with(".backblazeb2.com") {
        return Some("backblaze.com".to_string());
    }
    Some(host)
}

pub(crate) fn endpoint_host(endpoint_url: &str) -> String {
    let endpoint = endpoint_url.trim();
    if endpoint.is_empty() {
        return String::new();
    }
    let without_scheme = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .trim();
    if authority.starts_with('[') {
        return authority
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
    }
    authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(crate) fn is_private_or_local_host(host: &str) -> bool {
    if host == "localhost" || host == "::1" || host.ends_with(".local") {
        return true;
    }
    if host.starts_with("127.") || host.starts_with("10.") || host.starts_with("192.168.") {
        return true;
    }
    let mut octets = host.split('.');
    matches!(
        (
            octets.next(),
            octets.next().and_then(|value| value.parse::<u8>().ok())
        ),
        (Some("172"), Some(16..=31))
    )
}

pub(crate) fn s3_error<E: std::fmt::Display>(action: &str, err: E) -> ServerError {
    ServerError::App(format!("{action}: {err}"))
}

pub(crate) fn gcs_error<E: std::fmt::Display>(action: &str, err: E) -> ServerError {
    ServerError::App(format!("{action}: {err}"))
}

pub(crate) fn gcs_json_size(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(number) => number.as_i64().unwrap_or_default(),
        serde_json::Value::String(text) => text.parse().unwrap_or_default(),
        _ => 0,
    }
}

pub(crate) fn gcs_modified_label(update_time: Option<&str>) -> String {
    update_time.unwrap_or_default().to_string()
}

pub(crate) fn encode_uri_component(value: &str) -> String {
    utf8_percent_encode(value, GCS_JSON_PATH_ENCODE_SET).to_string()
}

pub(crate) trait ServerErrorContext {
    fn or_else_with_context(self, context: String) -> Self;
}

impl ServerErrorContext for ServerError {
    fn or_else_with_context(self, context: String) -> Self {
        match self {
            ServerError::App(message) => ServerError::App(format!("{message}; {context}")),
            other => other,
        }
    }
}

pub(crate) fn head_err_message<E: std::fmt::Display>(err: E) -> String {
    format!("enable create bucket or check the bucket name ({err})")
}

pub(crate) fn io_error(action: &str, err: std::io::Error) -> ServerError {
    ServerError::App(format!("{action}: {err}"))
}
