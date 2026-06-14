//! Container-registry resolution for adapters that don't own a registry.
//!
//! Render and Railway don't own a container registry, so their
//! adapters must pick a registry to push the image to. Precedence
//! (RFC 080 §11 q1):
//!
//! 1. an explicit `image_registry` from the `[deploy.<host>]` block;
//! 2. otherwise, derived from the detected git provider — GitHub →
//!    GitHub Container Registry, GitLab → GitLab Container Registry.
//!
//! The image an adapter pushes is `{namespace}/{app}:{sha}`.

use anyhow::{Result, bail};

/// A recognised git host, parsed from a remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitProvider {
    /// `github.com/<owner>/<repo>`.
    GitHub { owner: String, repo: String },
    /// `gitlab.com/<path>` — `path` is the full namespace
    /// (`group[/subgroup…]/project`); GitLab supports nested groups.
    GitLab { path: String },
    /// Anything else — a self-hosted host, Bitbucket, no remote, etc.
    Other,
}

impl GitProvider {
    /// Classify a git remote URL. Accepts `https://`, `http://`,
    /// `ssh://`, `git://`, and scp-style `git@host:path` forms; a
    /// trailing `.git` and surrounding slashes are ignored. Unrecognised
    /// or unparseable input yields [`GitProvider::Other`].
    pub fn from_remote(url: &str) -> GitProvider {
        let Some((host, path)) = parse_remote(url) else {
            return GitProvider::Other;
        };
        match host.as_str() {
            "github.com" => {
                let mut parts = path.splitn(2, '/');
                match (parts.next(), parts.next()) {
                    (Some(owner), Some(repo)) if !owner.is_empty() && !repo.is_empty() => {
                        GitProvider::GitHub {
                            owner: owner.to_owned(),
                            repo: repo.to_owned(),
                        }
                    }
                    _ => GitProvider::Other,
                }
            }
            "gitlab.com" => {
                // Needs at least `group/project`.
                if path.contains('/') {
                    GitProvider::GitLab { path }
                } else {
                    GitProvider::Other
                }
            }
            _ => GitProvider::Other,
        }
    }
}

/// The registry an adapter pushes to. The image is
/// `{namespace}/{app}:{sha}`; `host` is what `docker login` and the
/// host's pull-credential API are keyed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRegistry {
    /// e.g. `ghcr.io/myorg` or `registry.gitlab.com/group/project`.
    pub namespace: String,
    /// e.g. `ghcr.io` or `registry.gitlab.com`.
    pub host: String,
}

/// Resolve which registry to push to. `explicit` is the adapter's
/// `image_registry` override (if any); `git_remote` is the project's
/// `origin` URL. Errors when neither yields a registry.
pub fn resolve_registry(
    explicit: Option<&str>,
    git_remote: Option<&str>,
) -> Result<ResolvedRegistry> {
    // 1. An explicit override always wins.
    if let Some(reg) = explicit {
        let reg = reg.trim().trim_end_matches('/');
        if !reg.is_empty() {
            let host = reg.split('/').next().unwrap_or(reg).to_owned();
            return Ok(ResolvedRegistry {
                namespace: reg.to_owned(),
                host,
            });
        }
    }

    // 2. Derive from the git provider.
    match git_remote.map(GitProvider::from_remote) {
        Some(GitProvider::GitHub { owner, .. }) => Ok(ResolvedRegistry {
            namespace: format!("ghcr.io/{}", owner.to_lowercase()),
            host: "ghcr.io".to_owned(),
        }),
        Some(GitProvider::GitLab { path }) => Ok(ResolvedRegistry {
            namespace: format!("registry.gitlab.com/{}", path.to_lowercase()),
            host: "registry.gitlab.com".to_owned(),
        }),
        _ => bail!(
            "cannot determine a container registry: no `image_registry` set in the [deploy.<host>] block, and the git remote isn't a recognised GitHub/GitLab URL. Set `image_registry` explicitly, e.g. `image_registry = \"ghcr.io/myorg\"`.",
        ),
    }
}

/// Credentials a deploy host uses to pull a **private** image from the
/// registry. Adapters pass these to the host's API so a private image
/// needs no manual dashboard step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCredentials {
    pub username: String,
    pub token: String,
}

/// Resolve pull credentials for `registry`.
///
/// - **token** — an explicit long-lived token from the credentials
///   store (`pocopine deploy auth <host>` or `POCOPINE_<HOST>_TOKEN`).
///   Ambient CI tokens (`GITHUB_TOKEN`, `CI_JOB_TOKEN`) are
///   intentionally *not* used: the deploy host stores this credential
///   and would be left holding a dead token once the CI job ends.
/// - **username** — `explicit` (the `registry_username` override) if
///   set, else the GitHub owner derived from the remote (for GHCR).
///
/// Returns `None` when no token is available — the adapter then assumes
/// the image is public and wires no pull credential.
pub fn resolve_registry_credentials(
    registry: &ResolvedRegistry,
    explicit: Option<&str>,
    git_remote: Option<&str>,
) -> Option<RegistryCredentials> {
    let token = registry_token(&registry.host)?;
    let username = explicit
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .or_else(|| default_username(&registry.host, git_remote))?;
    Some(RegistryCredentials { username, token })
}

/// The registry token from the credentials store / `POCOPINE_<HOST>_TOKEN`
/// env (via [`crate::credentials::load`]). Deliberately does not fall
/// back to ambient CI tokens — see [`resolve_registry_credentials`].
fn registry_token(host: &str) -> Option<String> {
    crate::credentials::load(host)
        .ok()
        .filter(|t| !t.is_empty())
}

/// Best-effort default registry username. GHCR accepts the repo owner;
/// other registries have no derivable default (the `registry_username`
/// override is required there).
fn default_username(host: &str, git_remote: Option<&str>) -> Option<String> {
    match host {
        "ghcr.io" => match git_remote.map(GitProvider::from_remote) {
            Some(GitProvider::GitHub { owner, .. }) => Some(owner.to_lowercase()),
            _ => None,
        },
        _ => None,
    }
}

/// Split a git remote URL into `(host, path)` — host lower-cased, path
/// stripped of surrounding slashes and a trailing `.git`. Returns
/// `None` for input that isn't a recognisable remote.
///
/// URL forms (`https`, `http`, `ssh`, `git`) are parsed with the `url`
/// crate; scp-style `git@host:path` — which is not a URL, so `url`
/// can't parse it — is handled directly.
fn parse_remote(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim();

    let (host, path) = if remote.contains("://") {
        let parsed = url::Url::parse(remote).ok()?;
        (parsed.host_str()?.to_owned(), parsed.path().to_owned())
    } else {
        scp_style(remote)?
    };

    let host = normalize_host(&host);
    let path = normalize_path(&path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((host, path))
}

/// scp-style `[user@]host:path` → `(host, path)`.
fn scp_style(remote: &str) -> Option<(String, String)> {
    let (host_part, path) = remote.split_once(':')?;
    let host = host_part.rsplit('@').next().unwrap_or(host_part);
    Some((host.to_owned(), path.to_owned()))
}

/// Lower-case the host and drop any `:port`.
fn normalize_host(host: &str) -> String {
    host.split(':').next().unwrap_or("").trim().to_lowercase()
}

/// Drop surrounding slashes and a trailing `.git`.
fn normalize_path(path: &str) -> String {
    let p = path.trim().trim_start_matches('/').trim_end_matches('/');
    p.strip_suffix(".git").unwrap_or(p).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_https_and_ssh() {
        let expect = GitProvider::GitHub {
            owner: "mambisi".into(),
            repo: "pocopine".into(),
        };
        assert_eq!(
            GitProvider::from_remote("https://github.com/mambisi/pocopine.git"),
            expect,
        );
        assert_eq!(
            GitProvider::from_remote("https://github.com/mambisi/pocopine"),
            expect,
        );
        assert_eq!(
            GitProvider::from_remote("git@github.com:mambisi/pocopine.git"),
            expect,
        );
        assert_eq!(
            GitProvider::from_remote("ssh://git@github.com/mambisi/pocopine.git"),
            expect,
        );
    }

    #[test]
    fn parses_gitlab_with_nested_groups() {
        assert_eq!(
            GitProvider::from_remote("https://gitlab.com/group/sub/proj.git"),
            GitProvider::GitLab {
                path: "group/sub/proj".into()
            },
        );
        assert_eq!(
            GitProvider::from_remote("git@gitlab.com:group/proj.git"),
            GitProvider::GitLab {
                path: "group/proj".into()
            },
        );
    }

    #[test]
    fn unrecognised_hosts_are_other() {
        assert_eq!(
            GitProvider::from_remote("https://bitbucket.org/team/repo.git"),
            GitProvider::Other,
        );
        assert_eq!(
            GitProvider::from_remote("git@git.example.com:team/repo.git"),
            GitProvider::Other,
        );
        assert_eq!(GitProvider::from_remote("not a url"), GitProvider::Other);
        assert_eq!(GitProvider::from_remote(""), GitProvider::Other);
    }

    #[test]
    fn explicit_registry_wins_over_git_provider() {
        let r = resolve_registry(
            Some("ghcr.io/myorg"),
            Some("https://gitlab.com/group/proj.git"),
        )
        .unwrap();
        assert_eq!(r.namespace, "ghcr.io/myorg");
        assert_eq!(r.host, "ghcr.io");
    }

    #[test]
    fn explicit_registry_trailing_slash_trimmed() {
        let r = resolve_registry(Some("ghcr.io/myorg/"), None).unwrap();
        assert_eq!(r.namespace, "ghcr.io/myorg");
        assert_eq!(r.host, "ghcr.io");
    }

    #[test]
    fn github_remote_derives_ghcr_namespace() {
        let r = resolve_registry(None, Some("git@github.com:Mambisi/Pocopine.git")).unwrap();
        // Registry paths are lower-case.
        assert_eq!(r.namespace, "ghcr.io/mambisi");
        assert_eq!(r.host, "ghcr.io");
    }

    #[test]
    fn gitlab_remote_derives_gitlab_registry_namespace() {
        let r = resolve_registry(None, Some("https://gitlab.com/group/sub/proj.git")).unwrap();
        assert_eq!(r.namespace, "registry.gitlab.com/group/sub/proj");
        assert_eq!(r.host, "registry.gitlab.com");
    }

    #[test]
    fn empty_explicit_falls_through_to_git_provider() {
        let r = resolve_registry(Some("  "), Some("https://github.com/o/r.git")).unwrap();
        assert_eq!(r.namespace, "ghcr.io/o");
    }

    #[test]
    fn unresolvable_registry_errors_with_guidance() {
        let err = resolve_registry(None, Some("https://bitbucket.org/t/r.git"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("image_registry"));
        let err = resolve_registry(None, None).unwrap_err().to_string();
        assert!(err.contains("image_registry"));
    }

    #[test]
    fn resolve_creds_resolves_token_and_username() {
        let reg = ResolvedRegistry {
            namespace: "ghcr.io/acme".into(),
            host: "ghcr.io".into(),
        };
        // SAFETY: test-only env mutation.
        unsafe { std::env::set_var("POCOPINE_GHCR_IO_TOKEN", "tok-123") };

        // Explicit username wins.
        let c = resolve_registry_credentials(&reg, Some("ci-bot"), None).expect("resolved");
        assert_eq!(c.username, "ci-bot");
        assert_eq!(c.token, "tok-123");

        // No explicit username → GitHub owner derived from the remote.
        let c = resolve_registry_credentials(&reg, None, Some("https://github.com/Acme/app.git"))
            .expect("resolved");
        assert_eq!(c.username, "acme");

        // SAFETY: test-only env mutation.
        unsafe { std::env::remove_var("POCOPINE_GHCR_IO_TOKEN") };
    }

    #[test]
    fn resolve_creds_none_without_a_token() {
        // A registry host with no stored credentials and no env fallback.
        let reg = ResolvedRegistry {
            namespace: "registry.invalid/x".into(),
            host: "registry.invalid".into(),
        };
        assert!(resolve_registry_credentials(&reg, Some("u"), None).is_none());
    }
}
