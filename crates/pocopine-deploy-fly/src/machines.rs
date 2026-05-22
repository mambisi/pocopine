//! Fly Machines API client (`api.machines.dev` v1).
//!
//! Synchronous `reqwest::blocking` client used by [`crate::FlyAdapter`].
//! Bearer-token auth via `FLY_API_TOKEN` or
//! `~/.pocopine/credentials.toml` (loaded by `pocopine-deploy`'s
//! credentials store in Phase 2).
//!
//! Phase 1 scope: app and machine CRUD plus the `/wait` operation.
//! Volumes, secrets, and certificates land in follow-up phases when
//! the adapter actually needs them.
//!
//! Endpoint shapes (paths, request/response field names) are pinned to
//! the OpenAPI spec at `tmp/fly/openapi.json` (Swagger 2.0). When that
//! spec moves, this module needs to follow — see RFC 080 §11 q7 for
//! the `tested_against` schema-version warning path that surfaces drift.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

pub const FLY_API_BASE: &str = "https://api.machines.dev/v1";
pub const FLY_REGISTRY: &str = "registry.fly.io";

const DEFAULT_TIMEOUT_SECS: u64 = 60;

pub struct MachinesClient {
    base_url: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl MachinesClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base_url(FLY_API_BASE, token)
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

    // ─── Apps ───────────────────────────────────────────────────────────

    /// POST /apps — create a new Fly app under the given organisation.
    /// Returns Ok(()) on success; on 4xx/5xx, surfaces the body verbatim.
    pub fn create_app(&self, req: &CreateAppRequest) -> Result<()> {
        let resp = self
            .req(reqwest::Method::POST, "/apps")
            .json(req)
            .send()
            .context("fly create_app: request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            bail!("fly create_app failed ({status}): {body}");
        }
        Ok(())
    }

    /// GET /apps/{name}. Returns `Ok(None)` on 404 so callers can implement
    /// idempotent "ensure" logic without parsing error bodies.
    pub fn get_app(&self, name: &str) -> Result<Option<App>> {
        let path = format!("/apps/{name}");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .send()
            .context("fly get_app: request failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status().context("fly get_app")?;
        Ok(Some(resp.json().context("fly get_app: parse response")?))
    }

    /// Idempotent: returns the app if it exists, creates it if not. Used
    /// by the adapter's `deploy()` flow so first-deploy and re-deploy
    /// follow the same code path.
    pub fn ensure_app(&self, name: &str, org_slug: &str) -> Result<App> {
        if let Some(app) = self.get_app(name)? {
            info!(target: "pocopine.log", app = %name, "fly app exists");
            return Ok(app);
        }
        info!(target: "pocopine.log", app = %name, "fly app missing; creating");
        self.create_app(&CreateAppRequest {
            name: name.to_owned(),
            org_slug: org_slug.to_owned(),
            network: None,
            enable_subdomains: None,
        })?;
        self.get_app(name)?
            .context("fly: app still missing immediately after create")
    }

    // ─── Machines ───────────────────────────────────────────────────────

    /// GET /apps/{app}/machines.
    pub fn list_machines(&self, app: &str) -> Result<Vec<Machine>> {
        let path = format!("/apps/{app}/machines");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .send()
            .context("fly list_machines: request failed")?
            .error_for_status()
            .context("fly list_machines")?;
        resp.json().context("fly list_machines: parse response")
    }

    /// POST /apps/{app}/machines.
    pub fn create_machine(&self, app: &str, req: &CreateMachineRequest) -> Result<Machine> {
        let path = format!("/apps/{app}/machines");
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(req)
            .send()
            .context("fly create_machine: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly create_machine failed ({status}): {body}");
        }
        resp.json().context("fly create_machine: parse response")
    }

    /// POST /apps/{app}/machines/{machine_id} — update an existing
    /// machine in place. Fly's `UpdateMachineRequest` is a superset of
    /// `CreateMachineRequest`; we reuse the same body type and omit
    /// the optional `current_version` field for now (no optimistic
    /// concurrency on our writes yet).
    pub fn update_machine(
        &self,
        app: &str,
        machine_id: &str,
        req: &CreateMachineRequest,
    ) -> Result<Machine> {
        let path = format!("/apps/{app}/machines/{machine_id}");
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(req)
            .send()
            .context("fly update_machine: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly update_machine failed ({status}): {body}");
        }
        resp.json().context("fly update_machine: parse response")
    }

    /// DELETE /apps/{app}/machines/{machine_id}?force={force}. Treats 404
    /// as success so re-runs don't fail when the machine is already gone.
    pub fn destroy_machine(&self, app: &str, machine_id: &str, force: bool) -> Result<()> {
        let path = format!("/apps/{app}/machines/{machine_id}?force={force}");
        let resp = self
            .req(reqwest::Method::DELETE, &path)
            .send()
            .context("fly destroy_machine: request failed")?;
        let status = resp.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body = resp.text().unwrap_or_default();
        bail!("fly destroy_machine failed ({status}): {body}");
    }

    // ─── Volumes ─────────────────────────────────────────────────────────

    /// GET /apps/{app}/volumes — list all persistent volumes for an app.
    pub fn list_volumes(&self, app: &str) -> Result<Vec<Volume>> {
        let path = format!("/apps/{app}/volumes");
        let resp = self
            .req(reqwest::Method::GET, &path)
            .send()
            .context("fly list_volumes: request failed")?
            .error_for_status()
            .context("fly list_volumes")?;
        resp.json().context("fly list_volumes: parse response")
    }

    /// POST /apps/{app}/volumes — create a new persistent volume.
    /// The minimal body is `{ name, region, size_gb }`; everything
    /// else uses Fly's defaults until the adapter needs them.
    pub fn create_volume(&self, app: &str, req: &CreateVolumeRequest) -> Result<Volume> {
        let path = format!("/apps/{app}/volumes");
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(req)
            .send()
            .context("fly create_volume: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly create_volume failed ({status}): {body}");
        }
        resp.json().context("fly create_volume: parse response")
    }

    /// PUT /apps/{app}/volumes/{id}/extend — grow an existing volume in
    /// place. Fly extends online; the running machine sees the new size
    /// after the next reboot.
    pub fn extend_volume(&self, app: &str, volume_id: &str, size_gb: u32) -> Result<Volume> {
        let path = format!("/apps/{app}/volumes/{volume_id}/extend");
        let body = serde_json::json!({ "size_gb": size_gb });
        let resp = self
            .req(reqwest::Method::PUT, &path)
            .json(&body)
            .send()
            .context("fly extend_volume: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly extend_volume failed ({status}): {body}");
        }
        // The response wraps the volume; older API versions also wrap it
        // under `volume`. Decode laxly so both shapes work.
        let value: serde_json::Value = resp.json().context("fly extend_volume: parse response")?;
        let volume_value = value.get("volume").cloned().unwrap_or(value);
        let volume: Volume = serde_json::from_value(volume_value)
            .context("fly extend_volume: parse volume in response")?;
        Ok(volume)
    }

    /// Idempotent volume reconcile:
    ///
    /// - No existing volume with this `name`/`region` → create at `size_gb`.
    /// - Existing volume same size → reuse.
    /// - Existing volume **smaller** → extend in place to `size_gb`.
    /// - Existing volume **larger** → refuse: Fly volumes can't shrink,
    ///   and silently shipping a smaller declared size would hide a real
    ///   config mistake.
    pub fn ensure_volume(
        &self,
        app: &str,
        name: &str,
        region: &str,
        size_gb: u32,
    ) -> Result<Volume> {
        let existing = self.list_volumes(app)?;
        let found = existing
            .into_iter()
            .find(|v| v.name.as_deref() == Some(name) && v.region.as_deref() == Some(region));

        if let Some(v) = found {
            let current = v.size_gb.unwrap_or(0);
            if current == size_gb {
                return Ok(v);
            }
            if current < size_gb {
                return self.extend_volume(app, &v.id, size_gb);
            }
            bail!(
                "fly volume `{name}` exists at size {current} GiB but Pocopine.toml declares {size_gb} GiB. \
                 Fly volumes can't shrink; either restore the declared size to {current}+ GiB or delete the volume manually via the Fly console.",
            );
        }

        self.create_volume(
            app,
            &CreateVolumeRequest {
                name: name.to_owned(),
                region: region.to_owned(),
                size_gb,
            },
        )
    }

    // ─── Secrets ─────────────────────────────────────────────────────────

    /// POST /apps/{app}/secrets — set or overwrite app secrets. The body
    /// shape is `{ "values": { "KEY": "value", … } }` and the response
    /// includes a `version` integer that newly-created Machines can pin
    /// via `min_secrets_version`.
    ///
    /// Fly stores secrets encrypted at rest; values are not retrievable
    /// via the API. Use this for `DATABASE_URL`, API tokens, etc.,
    /// rather than putting them in `MachineConfig.env`.
    pub fn set_secrets(
        &self,
        app: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<SecretsUpdateResponse> {
        let path = format!("/apps/{app}/secrets");
        let body = serde_json::json!({ "values": values });
        let resp = self
            .req(reqwest::Method::POST, &path)
            .json(&body)
            .send()
            .context("fly set_secrets: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly set_secrets failed ({status}): {body}");
        }
        resp.json().context("fly set_secrets: parse response")
    }

    /// GET /apps/{app}/machines/{machine_id}/wait — block until the machine
    /// reaches `state` (e.g. `started`, `stopped`, `destroyed`) or the
    /// timeout elapses. `instance_id` pins the wait to a specific
    /// machine generation (Fly mutates `instance_id` on each update).
    ///
    /// The per-request timeout is sized to `timeout_secs` plus a small
    /// margin so the global 60 s client timeout doesn't tear down the
    /// connection before Fly has a chance to time out gracefully on its
    /// end.
    pub fn wait_machine(
        &self,
        app: &str,
        machine_id: &str,
        instance_id: &str,
        state: &str,
        timeout_secs: u32,
    ) -> Result<()> {
        let path = format!(
            "/apps/{app}/machines/{machine_id}/wait?instance_id={instance_id}&state={state}&timeout={timeout_secs}"
        );
        let resp = self
            .req(reqwest::Method::GET, &path)
            .timeout(Duration::from_secs(timeout_secs as u64 + 10))
            .send()
            .context("fly wait_machine: request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("fly wait_machine failed ({status}): {body}");
        }
        Ok(())
    }
}

// ─── Request / response types ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CreateAppRequest {
    /// Fly's `POST /apps` body field is `app_name`, not `name`.
    #[serde(rename = "app_name")]
    pub name: String,
    pub org_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_subdomains: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct App {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub machine_count: Option<u32>,
    #[serde(default)]
    pub volume_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateMachineRequest {
    pub name: String,
    pub region: String,
    pub config: MachineConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_launch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_service_registration: Option<bool>,
    /// Pin this machine to a specific `app.secrets` version. Set to the
    /// `version` returned by `set_secrets` so a freshly-created machine
    /// boots with the just-pushed values rather than a stale snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_secrets_version: Option<u32>,
}

/// Subset of `fly.MachineConfig` we currently emit. Fields are omitted
/// when empty so the wire format stays compact and the JSON diff at
/// review time matches what an adapter author wrote in code.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MachineConfig {
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init: Option<MachineInit>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<MachineService>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MachineMount>,
}

/// Fly's `fly.MachineInit` is an object — *not* a string array. Setting
/// `init.exec` overrides the image's CMD; `cmd` runs as the image's
/// default user; `entrypoint` overrides ENTRYPOINT. Only set the fields
/// the adapter actually needs; the rest are omitted on the wire.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MachineInit {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cmd: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoint: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exec: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MachineService {
    pub internal_port: u16,
    pub protocol: String,
    pub ports: Vec<MachinePort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
    /// `"suspend"`, `"stop"`, or `"off"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autostop: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<MachineServiceCheck>,
}

/// Fly service-level health check. Mirrors the `[[services.http_checks]]`
/// block in `fly.toml` and corresponds to `config.services.*.checks[*]`
/// in the Machines API.
#[derive(Debug, Clone, Serialize)]
pub struct MachineServiceCheck {
    /// `"http"` or `"tcp"`. Fly's API takes the discriminator under the
    /// `type` JSON key.
    #[serde(rename = "type")]
    pub kind: String,
    /// Probe interval, e.g. `"10s"`.
    pub interval: String,
    /// Per-probe timeout, e.g. `"5s"`.
    pub timeout: String,
    /// Grace period before checks are evaluated after start, e.g. `"5s"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_period: Option<String>,
    /// HTTP method (`"GET"` is the typical readiness probe).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Path on the service (e.g. `"/healthz"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MachinePort {
    pub port: u16,
    pub handlers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MachineMount {
    pub volume: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateVolumeRequest {
    pub name: String,
    pub region: String,
    pub size_gb: u32,
}

/// Subset of `fly.Volume`. The id is the canonical reference for
/// `MachineMount.volume`; the name + region are used by `ensure_volume`
/// to dedupe across runs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Volume {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub size_gb: Option<u32>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretsUpdateResponse {
    /// New secrets-version. Pin via `CreateMachineRequest.min_secrets_version`
    /// so freshly-created Machines see the latest values.
    #[serde(default)]
    pub version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Machine {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// Reduced view of `config` — we only need `metadata` for
    /// ownership checks during redeploy. The full `MachineConfig` is
    /// for serialisation (creation), not deserialisation.
    #[serde(default)]
    pub config: Option<MachineConfigSummary>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MachineConfigSummary {
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Machine {
    /// Returns the value of `config.metadata[key]` if present.
    pub fn metadata(&self, key: &str) -> Option<&str> {
        self.config
            .as_ref()
            .and_then(|c| c.metadata.get(key))
            .map(String::as_str)
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_joins_correctly() {
        let c = MachinesClient::with_base_url("https://api.machines.dev/v1", "tok");
        assert_eq!(c.url("/apps"), "https://api.machines.dev/v1/apps");
        assert_eq!(c.url("apps/foo"), "https://api.machines.dev/v1/apps/foo");
    }

    #[test]
    fn url_strips_trailing_slash_on_base() {
        let c = MachinesClient::with_base_url("https://api.machines.dev/v1/", "tok");
        assert_eq!(c.url("/apps"), "https://api.machines.dev/v1/apps");
    }

    #[test]
    fn create_app_request_serializes_compactly() {
        let req = CreateAppRequest {
            name: "myapp".into(),
            org_slug: "personal".into(),
            network: None,
            enable_subdomains: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        // Fly's `POST /apps` body field is `app_name`.
        assert_eq!(json, r#"{"app_name":"myapp","org_slug":"personal"}"#);
    }

    #[test]
    fn create_app_request_includes_optional_fields_when_set() {
        let req = CreateAppRequest {
            name: "myapp".into(),
            org_slug: "personal".into(),
            network: Some("default".into()),
            enable_subdomains: Some(true),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""network":"default""#));
        assert!(json.contains(r#""enable_subdomains":true"#));
    }

    #[test]
    fn machine_config_omits_empty_fields() {
        let config = MachineConfig {
            image: "registry.fly.io/myapp:sha".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        // Only `image` should be present on the wire.
        assert_eq!(json, r#"{"image":"registry.fly.io/myapp:sha"}"#);
    }

    #[test]
    fn machine_config_with_services_and_env() {
        let mut env = BTreeMap::new();
        env.insert("LOG_LEVEL".into(), "info".into());

        let config = MachineConfig {
            image: "registry.fly.io/myapp:sha".into(),
            env,
            services: vec![MachineService {
                internal_port: 8080,
                protocol: "tcp".into(),
                ports: vec![MachinePort {
                    port: 443,
                    handlers: vec!["tls".into(), "http".into()],
                }],
                autostart: Some(true),
                autostop: Some("suspend".into()),
                checks: vec![],
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""image":"registry.fly.io/myapp:sha""#));
        assert!(json.contains(r#""env":{"LOG_LEVEL":"info"}"#));
        assert!(json.contains(r#""internal_port":8080"#));
        assert!(json.contains(r#""handlers":["tls","http"]"#));
        assert!(json.contains(r#""autostop":"suspend""#));
        // Empty `checks` is omitted on the wire.
        assert!(!json.contains("\"checks\""));
    }

    #[test]
    fn machine_service_serializes_http_check() {
        let svc = MachineService {
            internal_port: 8080,
            protocol: "tcp".into(),
            ports: vec![],
            autostart: None,
            autostop: None,
            checks: vec![MachineServiceCheck {
                kind: "http".into(),
                interval: "10s".into(),
                timeout: "5s".into(),
                grace_period: Some("5s".into()),
                method: Some("GET".into()),
                path: Some("/healthz".into()),
            }],
        };
        let json = serde_json::to_string(&svc).unwrap();
        assert!(json.contains(r#""checks":[{"type":"http""#));
        assert!(json.contains(r#""interval":"10s""#));
        assert!(json.contains(r#""path":"/healthz""#));
    }

    #[test]
    fn app_response_deserializes_with_missing_fields() {
        let body = r#"{"name":"myapp"}"#;
        let app: App = serde_json::from_str(body).unwrap();
        assert_eq!(app.name, "myapp");
        assert!(app.id.is_none());
        assert!(app.status.is_none());
    }

    #[test]
    fn machine_init_serializes_as_object_not_array() {
        // Regression: Fly's `init` is an object, not a string array.
        // Earlier drafts modelled it as `Vec<String>` which would have
        // been rejected by api.machines.dev.
        let config = MachineConfig {
            image: "img".into(),
            init: Some(MachineInit {
                cmd: vec!["web".into()],
                entrypoint: vec!["/usr/local/bin/pocopine-launcher".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""init":{"cmd":["web"]"#));
        assert!(json.contains(r#""entrypoint":["/usr/local/bin/pocopine-launcher"]"#));
        // `exec` is empty → omitted.
        assert!(!json.contains("\"exec\""));
    }

    #[test]
    fn machine_init_omitted_when_none() {
        let config = MachineConfig {
            image: "img".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("\"init\""));
    }

    #[test]
    fn machine_response_deserializes() {
        let body =
            r#"{"id":"abc","name":"web-1","state":"started","instance_id":"01H","region":"fra"}"#;
        let machine: Machine = serde_json::from_str(body).unwrap();
        assert_eq!(machine.id, "abc");
        assert_eq!(machine.name, "web-1");
        assert_eq!(machine.state.as_deref(), Some("started"));
        assert!(machine.metadata("pocopine.process").is_none());
    }

    #[test]
    fn machine_response_carries_config_metadata() {
        let body = r#"{
            "id":"abc","name":"web-1",
            "config":{"metadata":{"pocopine.process":"web","owner":"me"}}
        }"#;
        let machine: Machine = serde_json::from_str(body).unwrap();
        assert_eq!(machine.metadata("pocopine.process"), Some("web"));
        assert_eq!(machine.metadata("missing"), None);
    }

    #[test]
    fn secrets_update_response_deserializes_with_version() {
        let body = r#"{"version": 7, "secrets": [{"name":"DATABASE_URL"}]}"#;
        let resp: SecretsUpdateResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.version, 7);
    }

    #[test]
    fn secrets_update_response_defaults_missing_version() {
        let body = r#"{"secrets": []}"#;
        let resp: SecretsUpdateResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.version, 0);
    }

    #[test]
    fn create_machine_request_includes_min_secrets_version_when_set() {
        let req = CreateMachineRequest {
            name: "web-1".into(),
            region: "fra".into(),
            config: MachineConfig {
                image: "img".into(),
                ..Default::default()
            },
            skip_launch: None,
            skip_service_registration: None,
            min_secrets_version: Some(7),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""min_secrets_version":7"#));
    }

    #[test]
    fn create_machine_request_omits_min_secrets_version_when_unset() {
        let req = CreateMachineRequest {
            name: "web-1".into(),
            region: "fra".into(),
            config: MachineConfig {
                image: "img".into(),
                ..Default::default()
            },
            skip_launch: None,
            skip_service_registration: None,
            min_secrets_version: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("min_secrets_version"));
    }

    #[test]
    fn create_volume_request_serializes() {
        let req = CreateVolumeRequest {
            name: "worker_data".into(),
            region: "fra".into(),
            size_gb: 10,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"name":"worker_data","region":"fra","size_gb":10}"#
        );
    }

    #[test]
    fn volume_response_deserializes_with_missing_optional_fields() {
        let body = r#"{"id":"vol_abc"}"#;
        let v: Volume = serde_json::from_str(body).unwrap();
        assert_eq!(v.id, "vol_abc");
        assert!(v.name.is_none());
        assert!(v.region.is_none());
    }
}
