//! Default in-memory artifact backend (tests and session-only harnesses), plus
//! the shared read-window/link helpers every backend uses.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use pocopine_codec::base64_encode;

use super::common::{
    ArtifactContentWindow, ArtifactDraft, ArtifactEncoding, ArtifactFuture, ArtifactMetadata,
    ArtifactScope, ArtifactStore, ArtifactStoreKind, MAX_READ_WINDOW_BYTES,
};

/// In-memory artifact store: metadata + owned contents in one table. Linked
/// artifacts re-read their workspace file at read time.
pub struct InMemoryArtifactStore {
    inner: Mutex<Inner>,
    workspace_root: Option<PathBuf>,
}

struct Inner {
    seq: u64,
    records: Vec<StoredArtifact>,
}

struct StoredArtifact {
    metadata: ArtifactMetadata,
    /// Owned contents; `None` for link references and tombstoned artifacts.
    contents: Option<Vec<u8>>,
}

impl InMemoryArtifactStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                seq: 0,
                records: Vec::new(),
            }),
            workspace_root: None,
        }
    }

    /// Set the workspace root linked artifacts resolve against.
    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }
}

impl Default for InMemoryArtifactStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactStore for InMemoryArtifactStore {
    fn write<'a>(
        &'a self,
        draft: ArtifactDraft,
        contents: Vec<u8>,
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            // A link derives size/hash from the live workspace file and never
            // owns bytes; an owned write stores what it was given.
            let contents = match &draft.link_path {
                Some(link_path) => read_linked_file(self.workspace_root.as_deref(), link_path)?,
                None => contents,
            };
            let mut inner = self.inner.lock().map_err(lock_err)?;
            inner.seq += 1;
            let metadata = ArtifactMetadata {
                id: format!("art-{}", inner.seq),
                name: draft.name,
                media_type: draft.media_type,
                size: contents.len() as u64,
                sha256: super::common::content_hash(&contents),
                scope: draft.scope,
                namespace: draft.namespace,
                source_refs: draft.source_refs,
                link_path: draft.link_path,
                created_at_ms: draft.created_at_ms,
                deleted: false,
            };
            let stored = if metadata.link_path.is_some() {
                None
            } else {
                Some(contents)
            };
            inner.records.push(StoredArtifact {
                metadata: metadata.clone(),
                contents: stored,
            });
            Ok(metadata)
        })
    }

    fn stat<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            let inner = self.inner.lock().map_err(lock_err)?;
            find_accessible(&inner.records, id, accessible).map(|record| record.metadata.clone())
        })
    }

    fn read<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
        offset: u64,
        max_bytes: usize,
    ) -> ArtifactFuture<'a, (ArtifactMetadata, ArtifactContentWindow)> {
        Box::pin(async move {
            let (metadata, owned) = {
                let inner = self.inner.lock().map_err(lock_err)?;
                let record = find_accessible(&inner.records, id, accessible)?;
                (record.metadata.clone(), record.contents.clone())
            };
            if metadata.deleted {
                return Err(deleted_err(id));
            }
            let bytes = match (&owned, &metadata.link_path) {
                (Some(bytes), _) => bytes.clone(),
                (None, Some(link_path)) => {
                    read_linked_file(self.workspace_root.as_deref(), link_path)?
                }
                (None, None) => return Err(deleted_err(id)),
            };
            let window = content_window(&bytes, offset, max_bytes);
            Ok((metadata, window))
        })
    }

    fn list<'a>(
        &'a self,
        accessible: &'a [(ArtifactScope, String)],
        scope: Option<ArtifactScope>,
        limit: usize,
    ) -> ArtifactFuture<'a, Vec<ArtifactMetadata>> {
        Box::pin(async move {
            let inner = self.inner.lock().map_err(lock_err)?;
            let mut listed: Vec<ArtifactMetadata> = inner
                .records
                .iter()
                .filter(|record| !record.metadata.deleted)
                .filter(|record| is_accessible(&record.metadata, accessible))
                .filter(|record| scope.is_none_or(|scope| record.metadata.scope == scope))
                .map(|record| record.metadata.clone())
                .collect();
            listed.reverse(); // newest first (append order)
            listed.truncate(limit);
            Ok(listed)
        })
    }

    fn delete<'a>(
        &'a self,
        id: &'a str,
        accessible: &'a [(ArtifactScope, String)],
    ) -> ArtifactFuture<'a, ArtifactMetadata> {
        Box::pin(async move {
            let mut inner = self.inner.lock().map_err(lock_err)?;
            let record = find_accessible_mut(&mut inner.records, id, accessible)?;
            if record.metadata.deleted {
                return Err(deleted_err(id));
            }
            record.metadata.deleted = true;
            record.contents = None;
            Ok(record.metadata.clone())
        })
    }

    fn kind(&self) -> ArtifactStoreKind {
        ArtifactStoreKind::InMemory
    }
}

fn find_accessible<'r>(
    records: &'r [StoredArtifact],
    id: &str,
    accessible: &[(ArtifactScope, String)],
) -> AgenkitResult<&'r StoredArtifact> {
    records
        .iter()
        .find(|record| record.metadata.id == id && is_accessible(&record.metadata, accessible))
        .ok_or_else(|| not_found(id))
}

fn find_accessible_mut<'r>(
    records: &'r mut [StoredArtifact],
    id: &str,
    accessible: &[(ArtifactScope, String)],
) -> AgenkitResult<&'r mut StoredArtifact> {
    records
        .iter_mut()
        .find(|record| record.metadata.id == id && is_accessible(&record.metadata, accessible))
        .ok_or_else(|| not_found(id))
}

/// A foreign artifact looks exactly like a missing one — no existence oracle.
pub(super) fn is_accessible(
    metadata: &ArtifactMetadata,
    accessible: &[(ArtifactScope, String)],
) -> bool {
    accessible
        .iter()
        .any(|(scope, namespace)| metadata.scope == *scope && metadata.namespace == *namespace)
}

pub(super) fn not_found(id: &str) -> AgenkitError {
    AgenkitError::not_found(format!("unknown artifact `{id}`"))
}

pub(super) fn deleted_err(id: &str) -> AgenkitError {
    AgenkitError::tool_policy(format!("artifact `{id}` was deleted"))
}

fn lock_err<T>(_err: std::sync::PoisonError<T>) -> AgenkitError {
    AgenkitError::internal("artifact store lock poisoned")
}

/// Cut a bounded window out of `bytes`: UTF-8 text when the slice is text,
/// base64 when it is genuinely binary. `max_bytes` is clamped to the
/// read-window cap.
///
/// A window boundary that lands mid-codepoint (the cap splits a multi-byte
/// char) does **not** flip a text artifact to base64: the trailing incomplete
/// bytes are dropped from this window — `end` retreats to the last char
/// boundary, so the split char returns whole in the next page — while a byte
/// that is invalid UTF-8 in the *middle* of the slice marks the content binary.
pub(super) fn content_window(bytes: &[u8], offset: u64, max_bytes: usize) -> ArtifactContentWindow {
    let start = (offset as usize).min(bytes.len());
    let max = max_bytes.clamp(1, MAX_READ_WINDOW_BYTES);
    let mut end = start.saturating_add(max).min(bytes.len());
    let slice = &bytes[start..end];
    let (content, encoding) = match std::str::from_utf8(slice) {
        Ok(text) => (text.to_string(), ArtifactEncoding::Utf8),
        Err(err) if err.error_len().is_none() && err.valid_up_to() > 0 => {
            // The only invalid bytes are an incomplete multi-byte char at the
            // very end: keep the valid text prefix and shrink the window so the
            // split char reappears whole next page (the caller's next offset is
            // `start + content.len()`).
            let valid = err.valid_up_to();
            let text =
                std::str::from_utf8(&slice[..valid]).expect("valid_up_to bytes are valid UTF-8");
            end = start + valid;
            (text.to_string(), ArtifactEncoding::Utf8)
        }
        Err(_) => (base64_encode(slice), ArtifactEncoding::Base64),
    };
    ArtifactContentWindow {
        offset: start as u64,
        content,
        encoding,
        truncated: end < bytes.len(),
    }
}

/// Resolve + read a linked workspace file. This is the single choke point for
/// link content — used at link creation *and* on every read (the live file
/// can change between the two) — so all three guards live here:
///
/// - **confinement**: the canonicalized target must stay under the
///   canonicalized workspace root (symlink escapes resolve outside);
/// - **secret-file policy**: the same path denial the fs tools enforce —
///   `artifact.link` must not become the side door to `.env` and friends —
///   plus the secret-content scan on the bytes (a benign-named file can hold
///   credential material, or gain it after linking);
/// - **bounded**: the file is stat-capped at [`MAX_CONTENT_BYTES`] before any
///   bytes load, so a multi-GB workspace file can neither be linked nor read.
pub(super) fn read_linked_file(
    workspace_root: Option<&Path>,
    link_path: &str,
) -> AgenkitResult<Vec<u8>> {
    let root = workspace_root.ok_or_else(|| {
        AgenkitError::config("artifact store has no workspace root for link references")
    })?;
    let resolved = resolve_link_target(root, link_path)?;
    let metadata = std::fs::metadata(&resolved)
        .map_err(|err| AgenkitError::internal(format!("stat linked artifact: {err}")))?;
    if !metadata.is_file() {
        return Err(AgenkitError::validation(format!(
            "linked path `{link_path}` is not a regular file"
        )));
    }
    let cap = super::common::MAX_CONTENT_BYTES;
    if metadata.len() > cap as u64 {
        return Err(AgenkitError::validation(format!(
            "linked file `{link_path}` exceeds the {cap} byte artifact cap"
        )));
    }
    // The `metadata.len()` check is advisory — the file can grow between stat
    // and read (a build log being appended). Read through a capped reader that
    // stops at `cap + 1` bytes and reject when the extra byte materializes, so
    // the cap is enforced on the bytes actually loaded, never bypassed.
    use std::io::Read;
    let file = std::fs::File::open(&resolved).map_err(|err| {
        AgenkitError::internal(format!("open linked artifact `{link_path}`: {err}"))
    })?;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            AgenkitError::internal(format!("read linked artifact `{link_path}`: {err}"))
        })?;
    if bytes.len() > cap {
        return Err(AgenkitError::validation(format!(
            "linked file `{link_path}` exceeds the {cap} byte artifact cap"
        )));
    }
    super::common::reject_secret_like_content(&bytes)?;
    Ok(bytes)
}

/// Canonicalize `root/link_path`, require it to stay under `root`, and apply
/// the fs family's secret-file path policy to the resolved target. The input
/// must be **workspace-relative** — an absolute path or a `..` component is
/// rejected before any join, so metadata never records a host-absolute path
/// (which would leak the local checkout location and break after a move) and
/// a `..`/absolute input can't sidestep the confinement check.
pub(super) fn resolve_link_target(root: &Path, link_path: &str) -> AgenkitResult<PathBuf> {
    let relative = Path::new(link_path);
    if relative.is_absolute() {
        return Err(AgenkitError::validation(format!(
            "artifact link path `{link_path}` must be workspace-relative, not absolute"
        )));
    }
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(AgenkitError::validation(format!(
            "artifact link path `{link_path}` must not traverse parent directories"
        )));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|err| AgenkitError::config(format!("canonicalize workspace root: {err}")))?;
    let candidate = root.join(relative);
    let resolved = std::fs::canonicalize(&candidate).map_err(|_| {
        AgenkitError::not_found(format!("linked file `{link_path}` does not exist"))
    })?;
    let Ok(under_root) = resolved.strip_prefix(&root) else {
        return Err(AgenkitError::tool_policy(format!(
            "linked path `{link_path}` escapes the workspace"
        )));
    };
    // Classify only the workspace-relative portion — matching the fs tools,
    // which strip the root first. Passing the absolute path would false-reject
    // every link when the checkout itself lives under a secret-looking dir
    // (e.g. `/tmp/.env-repro/project`). The canonicalized relative path still
    // catches an innocently-named symlink whose target is a secret file inside
    // the root.
    crate::tools::fs::common::reject_secret_path(under_root, "artifact.link")?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::super::common::current_time_ms;
    use super::*;

    fn draft(name: &str, scope: ArtifactScope, namespace: &str) -> ArtifactDraft {
        ArtifactDraft {
            name: name.to_string(),
            media_type: "text/plain".to_string(),
            scope,
            namespace: namespace.to_string(),
            source_refs: Vec::new(),
            link_path: None,
            created_at_ms: current_time_ms(),
        }
    }

    fn session_ns() -> Vec<(ArtifactScope, String)> {
        vec![(ArtifactScope::Session, "thread-1".to_string())]
    }

    #[tokio::test]
    async fn write_read_round_trips_with_hash_and_media_type() {
        let store = InMemoryArtifactStore::new();
        let meta = store
            .write(
                draft("report.md", ArtifactScope::Session, "thread-1"),
                b"# Report\nhello".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(meta.id, "art-1");
        assert_eq!(meta.size, 14);
        assert_eq!(
            meta.sha256,
            super::super::common::content_hash(b"# Report\nhello")
        );

        let (read_meta, window) = store.read("art-1", &session_ns(), 0, 1024).await.unwrap();
        assert_eq!(read_meta.media_type, "text/plain");
        assert_eq!(window.content, "# Report\nhello");
        assert_eq!(window.encoding, ArtifactEncoding::Utf8);
        assert!(!window.truncated);
    }

    #[tokio::test]
    async fn windows_paginate_and_binary_reads_as_base64() {
        let store = InMemoryArtifactStore::new();
        store
            .write(
                draft("blob.bin", ArtifactScope::Session, "thread-1"),
                vec![0xff, 0xfe, 0x01, 0x02],
            )
            .await
            .unwrap();
        let (_, window) = store.read("art-1", &session_ns(), 1, 2).await.unwrap();
        assert_eq!(window.encoding, ArtifactEncoding::Base64);
        assert_eq!(window.offset, 1);
        assert!(window.truncated);
    }

    #[tokio::test]
    async fn a_window_boundary_mid_codepoint_stays_text() {
        // "aé" — 'é' is two bytes (0xC3 0xA9). A 2-byte window from offset 0
        // ends in the MIDDLE of 'é'. A pure-text artifact must not flip to
        // base64: the window returns "a" (dropping the split char), and the
        // next window returns "é" whole.
        let store = InMemoryArtifactStore::new();
        store
            .write(
                draft("t.md", ArtifactScope::Session, "thread-1"),
                "aé".as_bytes().to_vec(),
            )
            .await
            .unwrap();
        let (_, first) = store.read("art-1", &session_ns(), 0, 2).await.unwrap();
        assert_eq!(first.encoding, ArtifactEncoding::Utf8);
        assert_eq!(first.content, "a"); // split char dropped from this window
        assert!(first.truncated);
        // The caller's next offset is start + the byte length it consumed.
        let next_offset = first.offset + first.content.len() as u64;
        assert_eq!(next_offset, 1);
        let (_, second) = store
            .read("art-1", &session_ns(), next_offset, 8)
            .await
            .unwrap();
        assert_eq!(second.encoding, ArtifactEncoding::Utf8);
        assert_eq!(second.content, "é");
        assert!(!second.truncated);
    }

    #[tokio::test]
    async fn foreign_namespaces_see_not_found_never_an_oracle() {
        let store = InMemoryArtifactStore::new();
        store
            .write(
                draft("private.md", ArtifactScope::Session, "thread-1"),
                b"x".to_vec(),
            )
            .await
            .unwrap();
        let foreign = vec![(ArtifactScope::Session, "thread-2".to_string())];
        assert_eq!(
            store.stat("art-1", &foreign).await.unwrap_err().kind(),
            "not_found"
        );
        assert_eq!(
            store
                .read("art-1", &foreign, 0, 16)
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
        assert_eq!(
            store.delete("art-1", &foreign).await.unwrap_err().kind(),
            "not_found"
        );
    }

    #[tokio::test]
    async fn delete_tombstones_and_drops_contents() {
        let store = InMemoryArtifactStore::new();
        store
            .write(
                draft("tmp.txt", ArtifactScope::Session, "thread-1"),
                b"x".to_vec(),
            )
            .await
            .unwrap();
        let deleted = store.delete("art-1", &session_ns()).await.unwrap();
        assert!(deleted.deleted);
        // Audit row survives; contents and re-delete do not.
        assert!(store.stat("art-1", &session_ns()).await.unwrap().deleted);
        assert_eq!(
            store
                .read("art-1", &session_ns(), 0, 16)
                .await
                .unwrap_err()
                .kind(),
            "tool_policy"
        );
        assert_eq!(
            store
                .delete("art-1", &session_ns())
                .await
                .unwrap_err()
                .kind(),
            "tool_policy"
        );
        // And it vanishes from listings.
        assert!(
            store
                .list(&session_ns(), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn linked_files_read_live_and_stay_confined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("out.log"), "line-1\n").unwrap();
        let store = InMemoryArtifactStore::new().with_workspace_root(dir.path());
        let mut link = draft("out.log", ArtifactScope::Session, "thread-1");
        link.link_path = Some("out.log".to_string());
        // The store derives size/hash from the linked file itself.
        let meta = store.write(link, Vec::new()).await.unwrap();
        assert_eq!(meta.size, 7);
        assert_eq!(meta.sha256, super::super::common::content_hash(b"line-1\n"));

        let (_, window) = store.read("art-1", &session_ns(), 0, 64).await.unwrap();
        assert_eq!(window.content, "line-1\n");

        // Escape attempts resolve outside the root and are rejected.
        let err = resolve_link_target(dir.path(), "../etc/passwd").unwrap_err();
        assert!(matches!(
            err.kind(),
            "tool_policy" | "not_found" | "validation"
        ));
    }

    #[test]
    fn link_paths_must_be_workspace_relative() {
        let dir = tempfile::tempdir().unwrap();
        // Absolute paths are refused even when they happen to sit under root —
        // metadata must never record a host-absolute path.
        let inside = dir.path().join("real").join("f.txt");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "x").unwrap();
        let err = resolve_link_target(dir.path(), inside.to_str().unwrap()).unwrap_err();
        assert_eq!(err.kind(), "validation");

        // `..` traversal is refused before any canonicalization.
        let err = resolve_link_target(dir.path(), "sub/../../escape").unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[test]
    fn secret_path_check_is_workspace_relative_not_absolute() {
        // The checkout lives under a secret-LOOKING ancestor dir. A perfectly
        // ordinary link target must NOT be rejected just because the workspace
        // root's absolute path contains a `.env`-shaped component.
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join(".env-repro").join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("report.md"), "ordinary content").unwrap();

        let resolved = resolve_link_target(&root, "report.md")
            .expect("a benign target under a secret-looking parent must resolve");
        assert!(resolved.ends_with("report.md"));

        // A genuinely secret target INSIDE the workspace is still rejected.
        std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
        assert_eq!(
            resolve_link_target(&root, ".env").unwrap_err().kind(),
            "tool_policy"
        );
    }

    #[tokio::test]
    async fn linked_file_growing_past_the_cap_is_rejected_on_read() {
        use super::super::common::MAX_CONTENT_BYTES;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build.log");
        std::fs::write(&path, b"small").unwrap();
        let store = InMemoryArtifactStore::new().with_workspace_root(dir.path());
        let mut link = draft("build.log", ArtifactScope::Session, "thread-1");
        link.link_path = Some("build.log".to_string());
        store.write(link, Vec::new()).await.unwrap();

        // The file grows past the cap after linking (a log being appended).
        // The capped reader enforces the limit on bytes actually read, even
        // though the earlier stat saw a small file.
        std::fs::write(&path, vec![b'x'; MAX_CONTENT_BYTES + 1]).unwrap();
        let err = store.read("art-1", &session_ns(), 0, 64).await.unwrap_err();
        assert_eq!(err.kind(), "validation");
        assert!(err.to_string().contains("artifact cap"));
    }

    #[tokio::test]
    async fn links_apply_the_fs_secret_file_policy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();
        let store = InMemoryArtifactStore::new().with_workspace_root(dir.path());

        // Linking a secret path is refused outright.
        let mut link = draft("env-copy", ArtifactScope::Session, "thread-1");
        link.link_path = Some(".env".to_string());
        let err = store.write(link, Vec::new()).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("secret-file policy"));

        // So is an innocently-named symlink whose target is the secret file.
        std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("notes.txt")).unwrap();
        let mut link = draft("notes.txt", ArtifactScope::Session, "thread-1");
        link.link_path = Some("notes.txt".to_string());
        let err = store.write(link, Vec::new()).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn linked_content_is_secret_scanned_at_link_and_at_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.log");
        std::fs::write(&path, "harmless output\n").unwrap();
        let store = InMemoryArtifactStore::new().with_workspace_root(dir.path());
        let mut link = draft("out.log", ArtifactScope::Session, "thread-1");
        link.link_path = Some("out.log".to_string());
        store.write(link, Vec::new()).await.unwrap();

        // The live file later gains credential material → reads refuse.
        std::fs::write(&path, "api_key = sk-live-12345\n").unwrap();
        let err = store.read("art-1", &session_ns(), 0, 64).await.unwrap_err();
        assert_eq!(err.kind(), "tool_policy");

        // Linking a file that already holds secrets is refused up front.
        std::fs::write(dir.path().join("creds.txt"), "api_key = sk-live-12345\n").unwrap();
        let mut link = draft("creds.txt", ArtifactScope::Session, "thread-1");
        link.link_path = Some("creds.txt".to_string());
        assert_eq!(
            store.write(link, Vec::new()).await.unwrap_err().kind(),
            "tool_policy"
        );
    }

    #[tokio::test]
    async fn oversized_linked_files_are_stat_capped_before_any_read() {
        use super::super::common::MAX_CONTENT_BYTES;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        // Sparse-extend past the cap without writing the bytes.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_CONTENT_BYTES as u64 + 1).unwrap();
        drop(file);

        let store = InMemoryArtifactStore::new().with_workspace_root(dir.path());
        let mut link = draft("huge.bin", ArtifactScope::Session, "thread-1");
        link.link_path = Some("huge.bin".to_string());
        let err = store.write(link, Vec::new()).await.unwrap_err();
        assert_eq!(err.kind(), "validation");
        assert!(err.to_string().contains("artifact cap"));
    }
}
