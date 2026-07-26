//! Cloudflare Pages v4 API client and Direct Upload protocol.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pocopine_codec::{QueryParams, base64_encode, percent_encode};
use pocopine_crypto::{Algorithm, Hasher, SecretString};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response, multipart};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use pocopine_deploy::{DeployState, ProcessStatus};

const DEFAULT_API_BASE: &str = "https://api.cloudflare.com/client/v4";
const RETRY_ATTEMPTS: usize = 5;

/// Cloudflare Pages' default Direct Upload asset-count limit.
pub const MAX_ASSET_COUNT: usize = 20_000;
/// Maximum size of a single Pages asset.
pub const MAX_ASSET_SIZE: u64 = 25 * 1024 * 1024;
/// Maximum asset count in one upload request.
pub const MAX_UPLOAD_BATCH_FILES: usize = 2_000;
/// Maximum raw asset-byte budget for one upload request.
pub const MAX_UPLOAD_BATCH_BYTES: usize = 40 * 1024 * 1024;

/// Small API-direct client used by [`crate::CloudflarePagesAdapter`].
pub struct PagesClient {
    http: Client,
    api_token: SecretString,
    api_base: String,
}

impl PagesClient {
    pub fn new(api_token: SecretString) -> Result<Self> {
        Self::with_base_url(api_token, DEFAULT_API_BASE)
    }

    /// Construct against an alternate API base (useful for an HTTP proxy or
    /// a protocol-level test server).
    pub fn with_base_url(api_token: SecretString, api_base: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!(
                "pocopine-deploy-cloudflare-pages/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .context("building Cloudflare Pages HTTP client")?;
        Ok(Self {
            http,
            api_token,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
        })
    }

    /// Resolve a Pages project, creating the Direct Upload project when it
    /// does not exist yet.
    pub fn ensure_project(
        &self,
        account_id: &str,
        project: &str,
        production_branch: &str,
    ) -> Result<Project> {
        if let Some(existing) = self.get_project(account_id, project)? {
            tracing::info!(
                target: "pocopine.log",
                "Cloudflare Pages project `{project}` exists"
            );
            return Ok(existing);
        }

        match self.create_project(account_id, project, production_branch) {
            Ok(created) => {
                tracing::info!(
                    target: "pocopine.log",
                    "created Cloudflare Pages project `{project}`"
                );
                Ok(created)
            }
            Err(create_error) => {
                // Another deploy may have created it between GET and POST.
                if let Ok(Some(existing)) = self.get_project(account_id, project) {
                    return Ok(existing);
                }
                Err(create_error)
            }
        }
    }

    pub fn get_project(&self, account_id: &str, project: &str) -> Result<Option<Project>> {
        let request = self.api_request(
            self.http.get(self.project_url(account_id, project)),
            "get Pages project",
        )?;
        let response = send_with_retry(request, "get Pages project")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        decode_envelope(response, "get Pages project").map(Some)
    }

    /// Upload one prepared static directory and create its Pages deployment.
    pub fn deploy_directory(
        &self,
        account_id: &str,
        project: &str,
        branch: &str,
        commit_hash: &str,
        dist: &Path,
    ) -> Result<Deployment> {
        let prepared = prepare_directory(dist)?;
        tracing::info!(
            target: "pocopine.log",
            "prepared {} Cloudflare Pages assets from {}",
            prepared.manifest.len(),
            dist.display()
        );

        let upload_token = self.upload_token(account_id, project)?;
        let all_hashes: Vec<String> = prepared.assets.keys().cloned().collect();
        let missing = self.check_missing(&upload_token, &all_hashes)?;
        let batches = upload_batches(&prepared.assets, &missing)?;
        let missing_count: usize = batches.iter().map(Vec::len).sum();
        tracing::info!(
            target: "pocopine.log",
            "Cloudflare Pages needs {missing_count}/{} unique assets in {} batch(es)",
            prepared.assets.len(),
            batches.len()
        );
        for (index, batch) in batches.iter().enumerate() {
            self.upload_batch(&upload_token, batch).with_context(|| {
                format!(
                    "uploading Cloudflare Pages asset batch {}/{}",
                    index + 1,
                    batches.len()
                )
            })?;
        }

        if let Err(error) = self.upsert_hashes(&upload_token, &all_hashes) {
            // Wrangler treats this cache hint as best-effort too; the final
            // manifest is authoritative and deployment may proceed.
            tracing::warn!(
                target: "pocopine.log",
                "Cloudflare Pages hash upsert failed (continuing): {error:#}"
            );
        }

        self.create_deployment(account_id, project, branch, commit_hash, &prepared)
    }

    /// Most recent deployment for a project, or `None` if the project or its
    /// deployment history does not exist yet.
    pub fn latest_deployment(
        &self,
        account_id: &str,
        project: &str,
        environment: Option<&str>,
    ) -> Result<Option<Deployment>> {
        if self.get_project(account_id, project)?.is_none() {
            return Ok(None);
        }
        let mut url = format!("{}/deployments", self.project_url(account_id, project));
        let mut query = QueryParams::new().pair("page", "1").pair("per_page", "1");
        if let Some(environment) = environment {
            query = query.pair("env", environment);
        }
        query.append_to(&mut url);
        let request = self.api_request(self.http.get(url), "list Pages deployments")?;
        let response = send_with_retry(request, "list Pages deployments")?;
        let mut deployments: Vec<Deployment> = decode_envelope(response, "list Pages deployments")?;
        Ok(deployments.drain(..).next())
    }

    fn create_project(
        &self,
        account_id: &str,
        project: &str,
        production_branch: &str,
    ) -> Result<Project> {
        let url = format!(
            "{}/accounts/{}/pages/projects",
            self.api_base,
            percent_encode(account_id)
        );
        let request = self
            .api_request(self.http.post(url), "create Pages project")?
            .json(&json!({
                "name": project,
                "production_branch": production_branch,
            }));
        let response = send_with_retry(request, "create Pages project")?;
        decode_envelope(response, "create Pages project")
    }

    fn upload_token(&self, account_id: &str, project: &str) -> Result<SecretString> {
        #[derive(Deserialize)]
        struct UploadToken {
            jwt: String,
        }

        let url = format!("{}/upload-token", self.project_url(account_id, project));
        let request = self.api_request(self.http.get(url), "get Pages upload token")?;
        let response = send_with_retry(request, "get Pages upload token")?;
        let result: UploadToken = decode_envelope(response, "get Pages upload token")?;
        if result.jwt.is_empty() {
            bail!("Cloudflare returned an empty Pages upload token");
        }
        Ok(SecretString::new(result.jwt))
    }

    fn check_missing(&self, upload_token: &SecretString, hashes: &[String]) -> Result<Vec<String>> {
        let request = self
            .upload_request(
                self.http
                    .post(format!("{}/pages/assets/check-missing", self.api_base)),
                upload_token,
            )
            .json(&json!({ "hashes": hashes }));
        let response = send_with_retry(request, "check missing Pages assets")?;
        decode_envelope(response, "check missing Pages assets")
    }

    fn upload_batch(&self, upload_token: &SecretString, assets: &[UploadAsset]) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AssetMetadata<'a> {
            content_type: &'a str,
        }
        #[derive(Serialize)]
        struct AssetBody<'a> {
            key: &'a str,
            value: String,
            metadata: AssetMetadata<'a>,
            base64: bool,
        }

        // Keep the inventory cheap: load bytes only for hashes Cloudflare
        // reported missing, one bounded batch at a time.
        let mut body = Vec::with_capacity(assets.len());
        for asset in assets {
            let bytes = fs::read(&asset.path)
                .with_context(|| format!("reading missing Pages asset {}", asset.path.display()))?;
            let value = base64_encode(&bytes);
            let extension = asset
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default();
            if pages_asset_digest(&value, extension) != asset.full_hash {
                bail!(
                    "Pages asset changed after manifest preparation: {}",
                    asset.path.display()
                );
            }
            body.push(AssetBody {
                key: &asset.hash,
                value,
                metadata: AssetMetadata {
                    content_type: &asset.content_type,
                },
                base64: true,
            });
        }
        let request = self
            .upload_request(
                self.http
                    .post(format!("{}/pages/assets/upload", self.api_base)),
                upload_token,
            )
            .json(&body);
        let response = send_with_retry(request, "upload Pages assets")?;
        let _: Value = decode_envelope(response, "upload Pages assets")?;
        Ok(())
    }

    fn upsert_hashes(&self, upload_token: &SecretString, hashes: &[String]) -> Result<()> {
        let request = self
            .upload_request(
                self.http
                    .post(format!("{}/pages/assets/upsert-hashes", self.api_base)),
                upload_token,
            )
            .json(&json!({ "hashes": hashes }));
        let response = send_with_retry(request, "upsert Pages asset hashes")?;
        let _: Value = decode_envelope(response, "upsert Pages asset hashes")?;
        Ok(())
    }

    fn create_deployment(
        &self,
        account_id: &str,
        project: &str,
        branch: &str,
        commit_hash: &str,
        prepared: &PreparedUpload,
    ) -> Result<Deployment> {
        let manifest =
            serde_json::to_string(&prepared.manifest).context("serialising Pages manifest")?;
        let mut form = multipart::Form::new()
            .text("manifest", manifest)
            .text("branch", branch.to_owned())
            .text("commit_hash", commit_hash.to_owned())
            .text("commit_dirty", "false");
        if let Some(headers) = &prepared.headers {
            form = form.part(
                "_headers",
                multipart::Part::bytes(headers.clone().into_bytes())
                    .file_name("_headers")
                    .mime_str("text/plain")
                    .context("building Pages _headers multipart field")?,
            );
        }
        if let Some(redirects) = &prepared.redirects {
            form = form.part(
                "_redirects",
                multipart::Part::bytes(redirects.clone().into_bytes())
                    .file_name("_redirects")
                    .mime_str("text/plain")
                    .context("building Pages _redirects multipart field")?,
            );
        }

        let url = format!("{}/deployments", self.project_url(account_id, project));
        let response = self
            .api_request(self.http.post(url), "create Pages deployment")?
            .multipart(form)
            .send()
            .context("calling Cloudflare to create Pages deployment")?;
        decode_envelope(response, "create Pages deployment")
    }

    fn project_url(&self, account_id: &str, project: &str) -> String {
        format!(
            "{}/accounts/{}/pages/projects/{}",
            self.api_base,
            percent_encode(account_id),
            percent_encode(project)
        )
    }

    fn api_request(&self, request: RequestBuilder, operation: &str) -> Result<RequestBuilder> {
        if self.api_token.expose().is_empty() {
            bail!("cannot {operation}: Cloudflare API token is empty");
        }
        Ok(request.bearer_auth(self.api_token.expose()))
    }

    fn upload_request(&self, request: RequestBuilder, token: &SecretString) -> RequestBuilder {
        request.bearer_auth(token.expose())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub subdomain: Option<String>,
    #[serde(default)]
    pub production_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deployment {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub latest_stage: Option<DeploymentStage>,
    #[serde(default)]
    pub created_on: Option<String>,
    #[serde(default)]
    pub modified_on: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeploymentStage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_on: Option<String>,
    #[serde(default)]
    pub ended_on: Option<String>,
}

/// Convert Cloudflare's deployment shape into the deploy contract's static
/// pseudo-process.
pub fn deployment_status(project: &str, deployment: Option<&Deployment>) -> ProcessStatus {
    let Some(deployment) = deployment else {
        return ProcessStatus {
            process: "static".into(),
            host_service_id: Some(project.into()),
            deploy_id: None,
            state: DeployState::Unknown,
            raw_state: String::new(),
            url: None,
            image: None,
            created_at: None,
            finished_at: None,
        };
    };

    let stage = deployment.latest_stage.as_ref();
    let raw_state = stage.map(|stage| stage.status.clone()).unwrap_or_default();
    let state = map_deploy_state(stage);
    ProcessStatus {
        process: "static".into(),
        host_service_id: Some(project.into()),
        deploy_id: Some(deployment.id.clone()),
        state,
        raw_state,
        url: Some(deployment.url.clone()),
        image: None,
        created_at: deployment
            .created_on
            .clone()
            .or_else(|| stage.and_then(|stage| stage.started_on.clone())),
        finished_at: stage
            .and_then(|stage| stage.ended_on.clone())
            .or_else(|| deployment.modified_on.clone()),
    }
}

fn map_deploy_state(stage: Option<&DeploymentStage>) -> DeployState {
    let Some(stage) = stage else {
        return DeployState::Unknown;
    };
    match stage.status.to_ascii_lowercase().as_str() {
        "success" => DeployState::Live,
        "failure" | "failed" => DeployState::Failed,
        "canceled" | "cancelled" => DeployState::Canceled,
        "queued" | "pending" | "idle" => DeployState::Pending,
        "active" | "running" | "in_progress" => {
            if stage.name.to_ascii_lowercase().contains("build") {
                DeployState::Building
            } else {
                DeployState::Deploying
            }
        }
        _ => DeployState::Unknown,
    }
}

#[derive(Debug)]
struct PreparedUpload {
    manifest: BTreeMap<String, String>,
    assets: BTreeMap<String, UploadAsset>,
    headers: Option<String>,
    redirects: Option<String>,
}

#[derive(Debug, Clone)]
struct UploadAsset {
    hash: String,
    full_hash: String,
    path: PathBuf,
    size_in_bytes: usize,
    content_type: String,
}

fn prepare_directory(root: &Path) -> Result<PreparedUpload> {
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("reading static dist {}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("static dist must be a real directory: {}", root.display());
    }
    for runtime_entry in ["_worker.js", "_worker.bundle", "_routes.json", "functions"] {
        if fs::symlink_metadata(root.join(runtime_entry)).is_ok() {
            bail!(
                "static cf-pages deployment does not accept `{runtime_entry}`; Workers/Functions artefacts require a separate deployment target"
            );
        }
    }

    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();

    let mut manifest = BTreeMap::new();
    let mut assets: BTreeMap<String, UploadAsset> = BTreeMap::new();
    let mut headers = None;
    let mut redirects = None;

    for file in files {
        let relative = file
            .strip_prefix(root)
            .expect("collected files remain beneath root");
        let relative_string = relative
            .to_str()
            .with_context(|| format!("Pages asset path is not UTF-8: {}", relative.display()))?
            .replace('\\', "/");

        if relative_string == ".DS_Store" || relative_string.ends_with("/.DS_Store") {
            continue;
        }
        match relative_string.as_str() {
            "_headers" => {
                headers = Some(fs::read_to_string(&file).with_context(|| {
                    format!("reading UTF-8 Pages _headers file {}", file.display())
                })?);
                continue;
            }
            "_redirects" => {
                redirects = Some(fs::read_to_string(&file).with_context(|| {
                    format!("reading UTF-8 Pages _redirects file {}", file.display())
                })?);
                continue;
            }
            _ => {}
        }

        if manifest.len() >= MAX_ASSET_COUNT {
            bail!(
                "Cloudflare Pages accepts at most {MAX_ASSET_COUNT} assets; `{}` would exceed the limit",
                file.display()
            );
        }
        let file_metadata = fs::metadata(&file)
            .with_context(|| format!("reading metadata for {}", file.display()))?;
        if file_metadata.len() > MAX_ASSET_SIZE {
            bail!(
                "Cloudflare Pages asset `{relative_string}` is {} bytes; maximum is {MAX_ASSET_SIZE} bytes",
                file_metadata.len()
            );
        }
        let bytes =
            fs::read(&file).with_context(|| format!("reading Pages asset {}", file.display()))?;
        let extension = relative
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();
        let value = base64_encode(&bytes);
        let full_hash = pages_asset_digest(&value, extension);
        let hash = full_hash[..32].to_owned();
        let content_type = mime_guess::from_path(relative)
            .first_or_octet_stream()
            .essence_str()
            .to_owned();
        let route = format!("/{relative_string}");

        if let Some(existing) = assets.get(&hash) {
            if existing.full_hash != full_hash || existing.content_type != content_type {
                bail!(
                    "Cloudflare Pages asset hash collision between `{route}` and another file ({hash})"
                );
            }
        } else {
            assets.insert(
                hash.clone(),
                UploadAsset {
                    hash: hash.clone(),
                    full_hash,
                    path: file,
                    size_in_bytes: bytes.len(),
                    content_type,
                },
            );
        }
        manifest.insert(route, hash);
    }

    if manifest.is_empty() {
        bail!(
            "static dist {} contains no deployable assets",
            root.display()
        );
    }
    if !manifest.contains_key("/index.html") {
        bail!(
            "static dist {} has no root index.html; include it in `[deploy].static_files`",
            root.display()
        );
    }

    Ok(PreparedUpload {
        manifest,
        assets,
        headers,
        redirects,
    })
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("reading static directory {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading entries in {}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("reading static path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing symlink in Cloudflare Pages dist: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            files.push(path);
        } else {
            bail!(
                "Cloudflare Pages dist contains a non-file entry: {}",
                path.display()
            );
        }
    }

    // `root` is carried explicitly to make the containment invariant clear
    // at the recursion boundary and guard future traversal changes.
    if !directory.starts_with(root) {
        bail!("static directory escaped its root: {}", directory.display());
    }
    Ok(())
}

/// Full digest behind the Pages content key. The wire key uses its first
/// 32 lowercase hex characters (128 bits).
fn pages_asset_digest(base64_value: &str, extension: &str) -> String {
    let mut hasher = Hasher::new(Algorithm::Blake3);
    hasher.update(base64_value.as_bytes());
    hasher.update(extension.as_bytes());
    hasher.finalize_hex()
}

fn upload_batches(
    assets: &BTreeMap<String, UploadAsset>,
    missing: &[String],
) -> Result<Vec<Vec<UploadAsset>>> {
    let mut missing = missing.iter().cloned().collect::<BTreeSet<_>>();
    let mut batches: Vec<Vec<UploadAsset>> = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0usize;

    for (hash, asset) in assets {
        if !missing.remove(hash) {
            continue;
        }
        let asset_size = asset.size_in_bytes;
        if !current.is_empty()
            && (current.len() >= MAX_UPLOAD_BATCH_FILES
                || current_bytes + asset_size > MAX_UPLOAD_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(asset.clone());
        current_bytes += asset_size;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    if !missing.is_empty() {
        bail!(
            "Cloudflare reported unknown missing asset hashes: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(batches)
}

#[derive(Debug, Deserialize)]
struct Envelope {
    success: bool,
    #[serde(default)]
    result: Value,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    errors: Vec<ApiMessage>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    messages: Vec<ApiMessage>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    #[serde(default)]
    code: Option<u64>,
    message: String,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn decode_envelope<T: DeserializeOwned>(response: Response, operation: &str) -> Result<T> {
    let status = response.status();
    let body = response
        .text()
        .with_context(|| format!("reading Cloudflare response for {operation}"))?;
    let envelope: Envelope = serde_json::from_str(&body).with_context(|| {
        format!(
            "Cloudflare returned invalid JSON for {operation} ({status}): {}",
            preview(&body)
        )
    })?;
    if !status.is_success() || !envelope.success {
        let details = envelope
            .errors
            .iter()
            .chain(envelope.messages.iter())
            .map(|message| match message.code {
                Some(code) => format!("{code}: {}", message.message),
                None => message.message.clone(),
            })
            .collect::<Vec<_>>()
            .join("; ");
        if details.is_empty() {
            bail!("Cloudflare {operation} failed with HTTP {status}");
        }
        bail!("Cloudflare {operation} failed with HTTP {status}: {details}");
    }
    serde_json::from_value(envelope.result)
        .with_context(|| format!("decoding Cloudflare result for {operation}"))
}

fn preview(body: &str) -> String {
    const LIMIT: usize = 400;
    if body.chars().count() <= LIMIT {
        return body.to_owned();
    }
    format!("{}…", body.chars().take(LIMIT).collect::<String>())
}

fn send_with_retry(request: RequestBuilder, operation: &str) -> Result<Response> {
    if request.try_clone().is_none() {
        return request
            .send()
            .with_context(|| format!("calling Cloudflare to {operation}"));
    }

    for attempt in 0..RETRY_ATTEMPTS {
        let attempt_request = request
            .try_clone()
            .expect("request cloneability checked before retry loop");
        match attempt_request.send() {
            Ok(response)
                if (response.status() == StatusCode::TOO_MANY_REQUESTS
                    || response.status().is_server_error())
                    && attempt + 1 < RETRY_ATTEMPTS =>
            {
                tracing::warn!(
                    target: "pocopine.log",
                    status = %response.status(),
                    attempt = attempt + 1,
                    "Cloudflare {operation} failed transiently; retrying"
                );
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt + 1 < RETRY_ATTEMPTS => {
                tracing::warn!(
                    target: "pocopine.log",
                    attempt = attempt + 1,
                    "Cloudflare {operation} request failed transiently; retrying: {error}"
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("calling Cloudflare to {operation}"));
            }
        }

        let delay_ms = 200u64 << attempt.min(3);
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
    unreachable!("retry loop either returns a response or the final error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_extensionless_asset_matches_pages_hash_vector() {
        assert_eq!(
            &pages_asset_digest("", "")[..32],
            "af1349b9f5f9a1a6a0404dea36dcc949"
        );
    }

    #[test]
    fn prepares_manifest_mime_and_control_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), "<h1>site</h1>").unwrap();
        fs::write(dir.path().join("app.wasm"), b"\0asm").unwrap();
        fs::write(dir.path().join("_headers"), "/*\n  X-Test: yes\n").unwrap();
        fs::write(dir.path().join("_redirects"), "/old /new 301\n").unwrap();

        let prepared = prepare_directory(dir.path()).unwrap();
        assert_eq!(prepared.manifest.len(), 2);
        assert!(prepared.manifest.contains_key("/index.html"));
        assert!(prepared.manifest.contains_key("/app.wasm"));
        assert_eq!(prepared.headers.as_deref(), Some("/*\n  X-Test: yes\n"));
        assert_eq!(prepared.redirects.as_deref(), Some("/old /new 301\n"));
        let wasm_hash = &prepared.manifest["/app.wasm"];
        assert_eq!(prepared.assets[wasm_hash].content_type, "application/wasm");
    }

    #[test]
    fn rejects_worker_control_files_in_static_mode() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), "site").unwrap();
        fs::write(dir.path().join("_worker.js"), "export default {}").unwrap();

        let error = prepare_directory(dir.path()).unwrap_err().to_string();
        assert!(error.contains("_worker.js"));
        assert!(error.contains("static"));
    }

    #[test]
    fn rejects_unknown_missing_hash() {
        let error = upload_batches(&BTreeMap::new(), &["unknown".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown"));
    }

    #[test]
    fn maps_cloudflare_status_to_static_process() {
        let deployment = Deployment {
            id: "dep-1".into(),
            url: "https://dep.pages.dev".into(),
            environment: Some("production".into()),
            aliases: Vec::new(),
            latest_stage: Some(DeploymentStage {
                name: "deploy".into(),
                status: "success".into(),
                started_on: None,
                ended_on: Some("2026-01-02T00:00:00Z".into()),
            }),
            created_on: Some("2026-01-01T00:00:00Z".into()),
            modified_on: None,
        };

        let status = deployment_status("site", Some(&deployment));
        assert_eq!(status.process, "static");
        assert_eq!(status.state, DeployState::Live);
        assert_eq!(status.deploy_id.as_deref(), Some("dep-1"));
    }
}
