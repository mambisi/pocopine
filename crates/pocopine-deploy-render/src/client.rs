//! Render REST API client (`api.render.com/v1`).
//!
//! Synchronous `reqwest::blocking` client used by [`crate::RenderAdapter`].
//! Bearer-token auth via `RENDER_API_KEY` env or
//! `~/.pocopine/credentials.toml`.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::info;

/// Read the response body as text and parse it as `T`, including the
/// status and the raw body in the error when parsing fails (or when the
/// body is empty). Used at every JSON-parse site so debug output always
/// surfaces what the host actually returned.
fn parse_json<T: DeserializeOwned>(resp: reqwest::blocking::Response, ctx: &str) -> Result<T> {
    let status = resp.status();
    let body = resp
        .text()
        .with_context(|| format!("{ctx}: reading response body (status {status})"))?;
    if body.trim().is_empty() {
        bail!("{ctx}: empty response body (status {status})");
    }
    serde_json::from_str(&body)
        .with_context(|| format!("{ctx}: parse failed (status {status}, body: {body})"))
}

pub const RENDER_API_BASE: &str = "https://api.render.com/v1";

const DEFAULT_TIMEOUT_SECS: u64 = 60;

pub struct RenderClient {
    base_url: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl RenderClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base_url(RENDER_API_BASE, token)
    }

    pub fn with_base_url(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .build()
                .expect("reqwest blocking client builds with default settings"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .bearer_auth(&self.token)
    }

    // ─── Services ────────────────────────────────────────────────────────

    /// GET /services?name={name} — list, filtering by exact name match.
    /// Render's API returns a list of `{ service, cursor }` envelopes;
    /// we flatten + filter to the exact match.
    pub fn find_service_by_name(&self, name: &str) -> Result<Option<Service>> {
        #[derive(Deserialize)]
        struct Envelope {
            service: Service,
        }

        let resp = self
            .req(reqwest::Method::GET, "/services")
            .query(&[("name", name), ("limit", "20")])
            .send()
            .context("render find_service_by_name: request failed")?
            .error_for_status()
            .context("render find_service_by_name")?;
        let envelopes: Vec<Envelope> = parse_json(resp, "render find_service_by_name")?;
        Ok(envelopes
            .into_iter()
            .map(|e| e.service)
            .find(|s| s.name == name))
    }

    /// GET /services/{id} — fetch a single service. Used to refresh the
    /// onrender.com URL after a deploy goes live (the URL isn't
    /// populated at create time).
    pub fn get_service(&self, service_id: &str) -> Result<Service> {
        let path = format!("/services/{service_id}");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .send()
            .context("render get_service: request failed")?
            .error_for_status()
            .context("render get_service")?;
        parse_json(resp, "render get_service")
    }

    /// POST /services — create a new service.
    pub fn create_service(&self, req: &CreateServiceRequest) -> Result<Service> {
        let body = req.to_render_body();
        info!(target: "pocopine.log", name = %req.name, "render create_service");
        let resp = self
            .req(reqwest::Method::POST, "/services")
            .json(&body)
            .send()
            .context("render create_service: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("render create_service failed ({status}): {body}");
        }
        #[derive(Deserialize)]
        struct CreateResp {
            service: Service,
        }
        let resp: CreateResp = parse_json(resp, "render create_service")?;
        Ok(resp.service)
    }

    /// PATCH /services/{id} — update the image URL (and, when the image
    /// is private, the registry-credential reference) for an existing
    /// image-backed service. Triggering a new deploy is separate (see
    /// `trigger_deploy`).
    pub fn update_service_image(
        &self,
        service_id: &str,
        image_url: &str,
        registry_credential_id: Option<&str>,
        owner_id: &str,
    ) -> Result<()> {
        let path = format!("/services/{service_id}");
        let mut image = serde_json::json!({
            "imagePath": image_url,
            "ownerId": owner_id,
        });
        if let Some(id) = registry_credential_id {
            image["registryCredentialId"] = serde_json::Value::String(id.to_owned());
        }
        let body = serde_json::json!({ "image": image });
        info!(target: "pocopine.log", service = %service_id, image = %image_url, "render update_service_image");
        let resp = self
            .req(reqwest::Method::PATCH, &path)
            .json(&body)
            .send()
            .context("render update_service_image: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("render update_service_image failed ({status}): {body}");
        }
        Ok(())
    }

    // ─── Registry credentials ────────────────────────────────────────────

    /// Find a registry credential by name; create it if absent, or
    /// update it (refreshing a rotated token) if found. Returns the
    /// credential id to attach to a service's `image` so Render can pull
    /// a private image — no dashboard step.
    pub fn ensure_registry_credential(&self, req: &RegistryCredentialRequest) -> Result<String> {
        let resp = self
            .req(reqwest::Method::GET, "/registrycredentials")
            .query(&[("limit", "100")])
            .send()
            .context("render ensure_registry_credential: list request failed")?
            .error_for_status()
            .context("render ensure_registry_credential: list")?;
        // List items are either flat credential objects or
        // `{ registryCredential, cursor }` envelopes — accept both.
        let items: Vec<serde_json::Value> =
            parse_json(resp, "render ensure_registry_credential: list")?;
        let existing_id = items.into_iter().find_map(|item| {
            let obj = item.get("registryCredential").unwrap_or(&item);
            let id = obj.get("id")?.as_str()?;
            let name = obj.get("name")?.as_str()?;
            (name == req.name).then(|| id.to_owned())
        });

        match existing_id {
            Some(id) => {
                // Refresh — the token may have rotated since last deploy.
                let path = format!("/registrycredentials/{id}");
                info!(target: "pocopine.log", name = %req.name, "render update registry credential");
                let resp = self
                    .req(reqwest::Method::PATCH, &path)
                    .json(&serde_json::json!({
                        "name": req.name,
                        "registry": req.registry,
                        "username": req.username,
                        "authToken": req.auth_token,
                    }))
                    .send()
                    .context("render ensure_registry_credential: update request failed")?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    bail!("render update registry credential failed ({status}): {body}");
                }
                Ok(id)
            }
            None => {
                info!(target: "pocopine.log", name = %req.name, "render create registry credential");
                let resp = self
                    .req(reqwest::Method::POST, "/registrycredentials")
                    .json(&req.to_create_body())
                    .send()
                    .context("render ensure_registry_credential: create request failed")?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().unwrap_or_default();
                    bail!("render create registry credential failed ({status}): {body}");
                }
                // Render returns the created credential, flat or under a
                // `registryCredential` envelope — accept both.
                let v: serde_json::Value =
                    parse_json(resp, "render ensure_registry_credential: create")?;
                let id = v
                    .get("id")
                    .or_else(|| v.get("registryCredential").and_then(|rc| rc.get("id")))
                    .and_then(|x| x.as_str())
                    .context("render create registry credential: response carried no id")?;
                Ok(id.to_owned())
            }
        }
    }

    // ─── Env vars ────────────────────────────────────────────────────────

    /// PUT /services/{id}/env-vars — replace the service's env-var
    /// set. Render encrypts values at rest, so this single endpoint
    /// handles both plain env and secrets. Values pushed here become
    /// the running container's env on next deploy.
    pub fn set_env_vars(&self, service_id: &str, vars: &[(String, String)]) -> Result<()> {
        let path = format!("/services/{service_id}/env-vars");
        #[derive(Serialize)]
        struct Entry<'a> {
            key: &'a str,
            value: &'a str,
        }
        let body: Vec<Entry<'_>> = vars
            .iter()
            .map(|(k, v)| Entry { key: k, value: v })
            .collect();
        info!(target: "pocopine.log", service = %service_id, n = body.len(), "render set_env_vars");
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&body)
            .send()
            .context("render set_env_vars: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let b = resp.text().unwrap_or_default();
            bail!("render set_env_vars failed ({status}): {b}");
        }
        Ok(())
    }

    // ─── Scale ───────────────────────────────────────────────────────────

    /// POST /services/{id}/scale — set the instance count.
    pub fn set_scale(&self, service_id: &str, num_instances: u32) -> Result<()> {
        let path = format!("/services/{service_id}/scale");
        let body = serde_json::json!({ "numInstances": num_instances });
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(&body)
            .send()
            .context("render set_scale: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let b = resp.text().unwrap_or_default();
            bail!("render set_scale failed ({status}): {b}");
        }
        Ok(())
    }

    // ─── Deploys ─────────────────────────────────────────────────────────

    /// POST /services/{id}/deploys — trigger a new deploy with the
    /// given image URL. Render queues a build/deploy and returns the
    /// `Deploy` record; poll with `wait_deploy`.
    ///
    /// Snapshots the prior latest-deploy id before POSTing so the
    /// empty-body fallback (Render sometimes responds 2xx with no body
    /// when a PATCH already queued a deploy) can wait for a *new*
    /// deployment matching the requested image, rather than accepting
    /// whatever `/deploys?limit=1` returns — which could be the prior
    /// `live` deploy and would cause `wait_deploy` to immediately
    /// report success for the old image tag.
    pub fn trigger_deploy(&self, service_id: &str, image_url: &str) -> Result<Deploy> {
        let prior_id = self.latest_deploy(service_id).ok().flatten().map(|d| d.id);
        let path = format!("/services/{service_id}/deploys");
        let body = serde_json::json!({ "imageUrl": image_url });
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(&body)
            .send()
            .context("render trigger_deploy: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let b = resp.text().unwrap_or_default();
            bail!("render trigger_deploy failed ({status}): {b}");
        }
        let status = resp.status();
        let body = resp
            .text()
            .context("render trigger_deploy: reading response body")?;
        if body.trim().is_empty() {
            info!(
                target: "pocopine.log",
                service = %service_id,
                status = %status,
                prior_deploy = ?prior_id,
                image = %image_url,
                "render trigger_deploy: empty body, polling for a new deploy with the requested image",
            );
            return self.poll_for_new_deploy(service_id, image_url, prior_id.as_deref());
        }
        let deploy: Deploy = serde_json::from_str(&body).with_context(|| {
            format!("render trigger_deploy: parse failed (status {status}, body: {body})")
        })?;
        Ok(deploy)
    }

    /// Wait for `latest_deploy` to return a Deploy that is strictly
    /// newer than `prior_id` and whose `image_ref` matches the
    /// requested `image_url`. Used by `trigger_deploy` when Render
    /// returned an empty 2xx body so we don't latch onto the previous
    /// `live` deploy. Caps at 30 s — long enough for Render to register
    /// the new deploy, short enough that a wedge surfaces quickly.
    fn poll_for_new_deploy(
        &self,
        service_id: &str,
        image_url: &str,
        prior_id: Option<&str>,
    ) -> Result<Deploy> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(latest) = self.latest_deploy(service_id)? {
                let id_is_new = prior_id.is_none_or(|p| latest.id != p);
                let image_matches = latest.image_ref() == Some(image_url);
                if id_is_new && image_matches {
                    return Ok(latest);
                }
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "render trigger_deploy: empty body and no new deploy with image `{image_url}` \
                     appeared within 30s (last seen latest: {prior_id:?}). Render may not have \
                     registered the new deploy yet; re-run or check the dashboard."
                );
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// GET /services/{id}/deploys?limit=1 — return the most recent deploy
    /// for a service, if any. Used as a fallback when `trigger_deploy`
    /// gets an empty 2xx body so the caller still has a deploy handle to
    /// poll.
    pub fn latest_deploy(&self, service_id: &str) -> Result<Option<Deploy>> {
        let path = format!("/services/{service_id}/deploys");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .query(&[("limit", "1")])
            .send()
            .context("render latest_deploy: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let b = resp.text().unwrap_or_default();
            bail!("render latest_deploy failed ({status}): {b}");
        }
        // Tolerate either an envelope list or a flat deploy list.
        let v: serde_json::Value = parse_json(resp, "render latest_deploy")?;
        let arr = v.as_array().cloned().unwrap_or_default();
        let Some(first) = arr.into_iter().next() else {
            return Ok(None);
        };
        let deploy_val = first.get("deploy").cloned().unwrap_or(first);
        let deploy: Deploy = serde_json::from_value(deploy_val.clone()).with_context(|| {
            format!("render latest_deploy: parse failed (body item: {deploy_val})")
        })?;
        Ok(Some(deploy))
    }

    /// GET /services/{id}/deploys/{deploy_id} — fetch a single deploy.
    pub fn get_deploy(&self, service_id: &str, deploy_id: &str) -> Result<Deploy> {
        let path = format!("/services/{service_id}/deploys/{deploy_id}");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .send()
            .context("render get_deploy: request failed")?
            .error_for_status()
            .context("render get_deploy")?;
        parse_json(resp, "render get_deploy")
    }

    /// Poll the deploy until it reaches `live` (success) or one of the
    /// terminal failure states. `timeout_secs` caps the overall wait;
    /// each poll uses a short HTTP timeout. A spinner ticks while
    /// waiting and the message reflects the latest Render status so the
    /// user can see progress through `build_in_progress` →
    /// `update_in_progress` → `live`.
    pub fn wait_deploy(
        &self,
        service_id: &str,
        deploy_id: &str,
        owner_id: &str,
        timeout_secs: u64,
    ) -> Result<()> {
        let style = indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner());
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(style);
        pb.enable_steady_tick(Duration::from_millis(120));
        pb.set_message(format!("render deploy {deploy_id}: queued"));

        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        let result = loop {
            let d = match self.get_deploy(service_id, deploy_id) {
                Ok(d) => d,
                Err(e) => break Err(e),
            };
            let status = d.status.as_deref().unwrap_or("(no status)");
            pb.set_message(format!("render deploy {deploy_id}: {status}"));
            match d.status.as_deref() {
                Some("live") => break Ok(()),
                Some(s) if is_terminal_failure(s) => {
                    break Err(anyhow::anyhow!(
                        "render deploy {deploy_id} ended in `{s}` state"
                    ));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                break Err(anyhow::anyhow!(
                    "render deploy {deploy_id} did not reach `live` within {timeout_secs}s (last status: {:?})",
                    d.status,
                ));
            }
            std::thread::sleep(Duration::from_secs(5));
        };

        match &result {
            Ok(()) => pb.finish_with_message(format!("render deploy {deploy_id}: live ✓")),
            Err(e) => pb.finish_with_message(format!("render deploy {deploy_id}: {e}")),
        }

        // On failure / timeout, dump the tail of Render's logs to
        // stderr so the user can diagnose without opening the
        // dashboard. Best-effort — printing the log error would just
        // bury the original deploy error.
        if result.is_err() {
            let logs = self.fetch_logs(owner_id, service_id, FAIL_LOG_LINES);
            print_render_logs(service_id, deploy_id, &logs);
        }
        result
    }

    /// Fetch the most recent `limit` log lines for a service via
    /// `GET /v1/logs?resource[]=<service_id>&direction=backward`.
    /// Returns an empty list on any error — caller is expected to be
    /// already reporting a primary failure and shouldn't double-fault.
    pub fn fetch_logs(&self, owner_id: &str, service_id: &str, limit: u32) -> Vec<RenderLog> {
        let resp = match self
            .req(reqwest::Method::GET, "/logs")
            .query(&[
                ("ownerId", owner_id),
                ("resource", service_id),
                ("limit", &limit.to_string()),
                ("direction", "backward"),
            ])
            .send()
        {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::debug!(
                    target: "pocopine.log",
                    status = %r.status(),
                    "render fetch_logs: non-success",
                );
                return Vec::new();
            }
            Err(e) => {
                tracing::debug!(target: "pocopine.log", error = %e, "render fetch_logs: request failed");
                return Vec::new();
            }
        };
        let body: serde_json::Value = match resp.json() {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let logs = body.get("logs").and_then(|l| l.as_array());
        let Some(logs) = logs else { return Vec::new() };
        // Render returns newest-first when direction=backward; flip so
        // callers see chronological order.
        let mut out: Vec<RenderLog> = logs
            .iter()
            .map(|l| RenderLog {
                timestamp: l
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                message: l
                    .get("message")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect();
        out.reverse();
        out
    }
}

/// Default tail size when a deploy fails / times out. Wide enough to
/// capture a typical Rust build error or panic trace, tight enough to
/// stay scannable in a terminal.
const FAIL_LOG_LINES: u32 = 80;

/// Dump the tail of Render logs for a failed deployment to stderr.
fn print_render_logs(service_id: &str, deploy_id: &str, logs: &[RenderLog]) {
    eprintln!();
    eprintln!("── render logs · service {service_id} · deploy {deploy_id} ──────────");
    if logs.is_empty() {
        eprintln!("(no logs returned — Render may not have captured any yet, or the credential lacks the `logs` scope)");
        return;
    }
    for entry in logs {
        if entry.timestamp.is_empty() {
            eprintln!("{}", entry.message);
        } else {
            eprintln!("{} {}", entry.timestamp, entry.message);
        }
    }
    eprintln!("── end logs ────────────────────────────────────");
    eprintln!();
}

/// Minimal Render log entry — the API has many more fields (labels,
/// instance id, etc.) that we don't surface to keep the failed-deploy
/// output scannable.
#[derive(Debug, Clone)]
pub struct RenderLog {
    pub timestamp: String,
    pub message: String,
}

fn is_terminal_failure(status: &str) -> bool {
    matches!(
        status,
        "build_failed"
            | "update_failed"
            | "canceled"
            | "deactivated"
            | "pre_deploy_failed"
            | "build_canceled"
    )
}

// ─── Request / response types ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CreateServiceRequest {
    pub name: String,
    pub owner_id: String,
    /// `"web_service"` or `"background_worker"` (subset we support).
    pub service_type: String,
    pub region: String,
    pub plan: String,
    pub image_url: String,
    /// Render registry-credential id for a private image; `None` =
    /// public image (Render pulls anonymously).
    pub registry_credential_id: Option<String>,
    pub env_vars: Vec<(String, String)>,
}

impl CreateServiceRequest {
    /// Convert to the JSON body Render expects (`servicePOST` in Render's
    /// OpenAPI spec). Per-type config goes under the single
    /// **`serviceDetails`** key — a `oneOf` discriminated by the
    /// top-level `type`. There is no top-level `webServiceDetails` /
    /// `backgroundWorkerDetails` key; we only build the subset we need
    /// (image runtime, web or worker).
    pub fn to_render_body(&self) -> serde_json::Value {
        // Render rejects an unknown `type`; guard before building.
        match self.service_type.as_str() {
            "web_service" | "background_worker" => {}
            other => panic!("CreateServiceRequest: unsupported service_type {other}"),
        }

        let env_vars: Vec<serde_json::Value> = self
            .env_vars
            .iter()
            .map(|(k, v)| serde_json::json!({ "key": k, "value": v }))
            .collect();

        let mut image = serde_json::json!({
            "imagePath": self.image_url,
            "ownerId": self.owner_id,
        });
        if let Some(id) = &self.registry_credential_id {
            image["registryCredentialId"] = serde_json::Value::String(id.clone());
        }

        serde_json::json!({
            "type": self.service_type,
            "name": self.name,
            "ownerId": self.owner_id,
            "envVars": env_vars,
            "image": image,
            "serviceDetails": {
                "runtime": "image",
                "region": self.region,
                "plan": self.plan,
            }
        })
    }
}

/// Request body for `POST` / `PATCH /registrycredentials`.
#[derive(Debug, Clone)]
pub struct RegistryCredentialRequest {
    /// Stable name so the credential is found-and-updated, not
    /// duplicated, on every deploy.
    pub name: String,
    /// Render registry kind: `GITHUB`, `GITLAB`, or `DOCKER`.
    pub registry: String,
    pub username: String,
    pub auth_token: String,
    pub owner_id: String,
}

impl RegistryCredentialRequest {
    /// `POST` body — includes `ownerId` (fixed at creation time).
    fn to_create_body(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "registry": self.registry,
            "username": self.username,
            "authToken": self.auth_token,
            "ownerId": self.owner_id,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    #[serde(default, rename = "type")]
    pub service_type: Option<String>,
    /// Some response shapes carry the public URL at the top level; most
    /// nest it under `serviceDetails.url`. Use [`Service::public_url`]
    /// rather than reading this field directly.
    #[serde(default)]
    pub url: Option<String>,
    /// Nested details Render returns on GET — includes the assigned
    /// onrender.com URL for web services.
    #[serde(default, rename = "serviceDetails")]
    pub service_details: Option<ServiceDetails>,
    /// Render dashboard URL for this service. Surfaced so the CLI can
    /// print a clickable link even before the public URL is assigned.
    #[serde(default, rename = "dashboardUrl")]
    pub dashboard_url: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceDetails {
    #[serde(default)]
    pub url: Option<String>,
}

impl Service {
    /// The public onrender.com URL Render has assigned, regardless of
    /// whether the response carries it at the top level or under
    /// `serviceDetails`. Returns `None` for background workers or
    /// before Render has finished provisioning the hostname.
    pub fn public_url(&self) -> Option<&str> {
        self.url
            .as_deref()
            .or_else(|| self.service_details.as_ref().and_then(|d| d.url.as_deref()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deploy {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    /// Image descriptor (Render returns `{ ref, sha }` for image-backed
    /// services). We surface the `ref` via [`Deploy::image_ref`].
    #[serde(default)]
    pub image: Option<DeployImage>,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<String>,
    #[serde(default, rename = "finishedAt")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeployImage {
    #[serde(default, rename = "ref")]
    pub image_ref: Option<String>,
    #[serde(default)]
    pub sha: Option<String>,
}

impl Deploy {
    /// Convenience: return the image ref (`ghcr.io/owner/app:tag`) if
    /// Render included one on this deploy.
    pub fn image_ref(&self) -> Option<&str> {
        self.image.as_ref().and_then(|i| i.image_ref.as_deref())
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_joins_correctly() {
        let c = RenderClient::with_base_url(RENDER_API_BASE, "tok");
        assert_eq!(c.url("/services"), "https://api.render.com/v1/services");
        assert_eq!(
            c.url("services/foo"),
            "https://api.render.com/v1/services/foo"
        );
    }

    #[test]
    fn create_service_body_nests_web_config_under_service_details() {
        let req = CreateServiceRequest {
            name: "test-app-web".into(),
            owner_id: "wrk-abc".into(),
            service_type: "web_service".into(),
            region: "oregon".into(),
            plan: "starter".into(),
            image_url: "ghcr.io/owner/app:sha".into(),
            registry_credential_id: None,
            env_vars: vec![("LOG_LEVEL".into(), "info".into())],
        };
        let body = req.to_render_body();
        assert_eq!(body["type"], "web_service");
        assert_eq!(body["name"], "test-app-web");
        assert_eq!(body["ownerId"], "wrk-abc");
        assert_eq!(body["image"]["imagePath"], "ghcr.io/owner/app:sha");
        assert_eq!(body["serviceDetails"]["runtime"], "image");
        assert_eq!(body["serviceDetails"]["region"], "oregon");
        assert_eq!(body["envVars"][0]["key"], "LOG_LEVEL");
        assert_eq!(body["envVars"][0]["value"], "info");
        // The legacy type-keyed shapes must NOT be present — Render's
        // OpenAPI uses a single `serviceDetails`.
        assert!(body.get("webServiceDetails").is_none());
        assert!(body.get("backgroundWorkerDetails").is_none());
    }

    #[test]
    fn create_service_body_nests_worker_config_under_service_details() {
        let req = CreateServiceRequest {
            name: "test-app-worker".into(),
            owner_id: "wrk-abc".into(),
            service_type: "background_worker".into(),
            region: "oregon".into(),
            plan: "starter".into(),
            image_url: "ghcr.io/owner/app:sha".into(),
            registry_credential_id: None,
            env_vars: vec![],
        };
        let body = req.to_render_body();
        assert_eq!(body["type"], "background_worker");
        assert_eq!(body["serviceDetails"]["runtime"], "image");
        assert!(body.get("backgroundWorkerDetails").is_none());
        assert!(body.get("webServiceDetails").is_none());
    }

    #[test]
    fn is_terminal_failure_classifier() {
        for s in [
            "build_failed",
            "update_failed",
            "canceled",
            "deactivated",
            "pre_deploy_failed",
            "build_canceled",
        ] {
            assert!(is_terminal_failure(s), "{s} should be terminal");
        }
        for s in ["build_in_progress", "live", "queued", "created"] {
            assert!(!is_terminal_failure(s), "{s} should not be terminal");
        }
    }

    #[test]
    fn create_service_body_carries_registry_credential_id_when_set() {
        let req = CreateServiceRequest {
            name: "app-web".into(),
            owner_id: "wrk".into(),
            service_type: "web_service".into(),
            region: "oregon".into(),
            plan: "starter".into(),
            image_url: "ghcr.io/owner/app:sha".into(),
            registry_credential_id: Some("rc-123".into()),
            env_vars: vec![],
        };
        let body = req.to_render_body();
        assert_eq!(body["image"]["imagePath"], "ghcr.io/owner/app:sha");
        assert_eq!(body["image"]["registryCredentialId"], "rc-123");
    }

    #[test]
    fn registry_credential_create_body_uses_render_field_names() {
        let body = RegistryCredentialRequest {
            name: "pocopine-ghcr.io".into(),
            registry: "GITHUB".into(),
            username: "octocat".into(),
            auth_token: "tok".into(),
            owner_id: "wrk".into(),
        }
        .to_create_body();
        assert_eq!(body["name"], "pocopine-ghcr.io");
        assert_eq!(body["registry"], "GITHUB");
        assert_eq!(body["username"], "octocat");
        assert_eq!(body["authToken"], "tok");
        assert_eq!(body["ownerId"], "wrk");
    }
}
