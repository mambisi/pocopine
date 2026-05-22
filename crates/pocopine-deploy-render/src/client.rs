//! Render REST API client (`api.render.com/v1`).
//!
//! Synchronous `reqwest::blocking` client used by [`crate::RenderAdapter`].
//! Bearer-token auth via `RENDER_API_KEY` env or
//! `~/.pocopine/credentials.toml`.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

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
        let envelopes: Vec<Envelope> = resp
            .json()
            .context("render find_service_by_name: parse response")?;
        Ok(envelopes
            .into_iter()
            .map(|e| e.service)
            .find(|s| s.name == name))
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
        let resp: CreateResp = resp
            .json()
            .context("render create_service: parse response")?;
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
        let items: Vec<serde_json::Value> = resp
            .json()
            .context("render ensure_registry_credential: parse list")?;
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
                let v: serde_json::Value = resp
                    .json()
                    .context("render ensure_registry_credential: parse create response")?;
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
    pub fn trigger_deploy(&self, service_id: &str, image_url: &str) -> Result<Deploy> {
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
        let deploy: Deploy = resp
            .json()
            .context("render trigger_deploy: parse response")?;
        Ok(deploy)
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
        resp.json().context("render get_deploy: parse response")
    }

    /// Poll the deploy until it reaches `live` (success) or one of the
    /// terminal failure states. `timeout_secs` caps the overall wait;
    /// each poll uses a short HTTP timeout.
    pub fn wait_deploy(&self, service_id: &str, deploy_id: &str, timeout_secs: u64) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let d = self.get_deploy(service_id, deploy_id)?;
            match d.status.as_deref() {
                Some("live") => return Ok(()),
                Some(s) if is_terminal_failure(s) => {
                    bail!("render deploy {deploy_id} ended in `{s}` state");
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "render deploy {deploy_id} did not reach `live` within {timeout_secs}s (last status: {:?})",
                    d.status,
                );
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }
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
    /// Public URL of the service (web services only).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Deploy {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
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
