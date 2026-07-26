//! `DeployAdapter` trait and the supporting enums every adapter consumes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Artefact, DeploySpec, StagedFiles};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterMode {
    Static,
    Fullstack,
    Both,
}

/// Result of `detect_constraints`. `Refuse` halts the deploy; `Warn`
/// and `Hint` are advisory.
#[derive(Debug, Clone)]
pub enum Constraint {
    Refuse(String),
    Warn(String),
    Hint(String),
}

/// Returned by `post_deploy_hint`. `OneTime` is multi-line setup the
/// user must run themselves (provisioning, secrets); `Info` is a
/// per-deploy banner (deployment URL, etc.).
#[derive(Debug, Clone)]
pub enum Hint {
    OneTime(String),
    Info(String),
}

#[derive(Debug, Clone)]
pub struct DeployOutcome {
    pub url: String,
    pub host_ids: Vec<String>,
}

/// Normalised deploy state for cross-host comparison. Adapters
/// translate their host-specific state strings into this enum and also
/// return the raw value on [`ProcessStatus::raw_state`] so callers can
/// inspect host-specific nuance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployState {
    /// Queued/accepted, no work started.
    Pending,
    /// Building image or running pre-deploy steps.
    Building,
    /// Image built, rolling out (pulling, starting instances).
    Deploying,
    /// Serving traffic.
    Live,
    /// Terminal failure.
    Failed,
    /// User-canceled or superseded.
    Canceled,
    /// Host returned a state we don't recognise — see `raw_state`.
    Unknown,
}

/// Per-process status entry returned by [`DeployAdapter::status`]. Fullstack
/// adapters return one per declared process, in declaration order. Static
/// adapters return one synthetic process named `"static"`. Processes that
/// have never been deployed appear with `deploy_id: None` and
/// `state: Unknown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatus {
    /// Logical process name from the spec (e.g. `"web"`, `"worker"`).
    pub process: String,
    /// Host-side service identifier (e.g. Render `srv-XXXX`, Railway
    /// service id). `None` if the host has no record of this process
    /// yet.
    pub host_service_id: Option<String>,
    /// Most recent deploy id, if any.
    pub deploy_id: Option<String>,
    /// Normalised state for cross-host comparison.
    pub state: DeployState,
    /// Host-specific state string verbatim (e.g. `"build_in_progress"`).
    /// Empty when the host has no deploy on record.
    pub raw_state: String,
    /// Public URL, if the host exposes one.
    pub url: Option<String>,
    /// Image reference this status describes.
    pub image: Option<String>,
    /// ISO-8601 timestamps when the host returns them.
    pub created_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AdapterSource {
    Builtin,
    /// Reserved for future use. RFC 080 §3 explicitly rejects a
    /// plugin protocol; this variant exists so the trait can describe
    /// non-builtin sources should that ever change.
    External {
        path: PathBuf,
    },
}

/// The trait every adapter implements (RFC 080 §5.2).
pub trait DeployAdapter {
    fn name(&self) -> &'static str;
    fn mode(&self) -> AdapterMode;

    /// Host-API schema/version range this adapter is known to be
    /// compatible with. `pocopine deploy doctor` probes the host's
    /// version endpoint and emits a `Warn` outside this range.
    fn tested_against(&self) -> semver::VersionReq;

    /// Pure: no I/O. Errors here halt before we build a 200 MB image.
    fn detect_constraints(&self, spec: &DeploySpec) -> Vec<Constraint>;

    /// Pure: writes files to a staging dir. The orchestrator decides
    /// whether to flush them, diff against existing, or just print
    /// (for `--dry-run`).
    fn render_config(&self, spec: &DeploySpec, out: &mut StagedFiles);

    /// I/O: invokes `docker build` (fullstack) or copies `dist/` (static).
    fn build_artefact(&self, spec: &DeploySpec) -> anyhow::Result<Artefact>;

    /// Pure: the artefact that `build_artefact` *would* produce for this
    /// spec, without doing the build. Used by
    /// `pocopine deploy --skip-build` to reuse an image that's already
    /// in the host's registry (CI's build-once-deploy-many pattern).
    /// Adapters with a deterministic image-tag scheme (most of them)
    /// override this; the default returns a generic local tag that
    /// rarely matches a real registry.
    fn default_artefact(&self, spec: &DeploySpec) -> Artefact {
        Artefact::OciImage {
            tag: format!("pocopine-{}:{}", spec.app_name, spec.git_sha),
        }
    }

    /// I/O: calls host API (HTTP/GraphQL), pushes image to host registry.
    /// No host CLI shell-out.
    fn deploy(&self, spec: &DeploySpec, artefact: &Artefact) -> anyhow::Result<DeployOutcome>;

    /// Pure: returns follow-up commands the user must run once
    /// (one-time provisioning prompts) and informational lines
    /// (deployment URL, etc.). The adapter never runs these.
    fn post_deploy_hint(&self, spec: &DeploySpec, outcome: &DeployOutcome) -> Vec<Hint>;

    /// I/O: query the host API for the current deploy state. No build or
    /// push. Fullstack adapters return one [`ProcessStatus`] per process in
    /// declaration order; static adapters return one synthetic `"static"`
    /// process. A target that has never been deployed appears with
    /// `deploy_id: None` and `state: Unknown` rather than as an error.
    fn status(&self, spec: &DeploySpec) -> anyhow::Result<Vec<ProcessStatus>>;

    fn source(&self) -> AdapterSource {
        AdapterSource::Builtin
    }
}
