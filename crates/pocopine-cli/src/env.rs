//! Project-root `.env` file management for `pocopine env set/get/list/unset`
//! and the dev-mode env loader.
//!
//! The file format is the standard dotenv shape:
//!
//! ```ignore
//! # comment
//! KEY=value
//! QUOTED="value with spaces"
//! ```
//!
//! Values are loaded into child processes spawned by `pocopine dev` only —
//! `pocopine run` (the production-shape mode) inherits the parent
//! environment unchanged so dev convenience vars never leak into prod.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Standard `.env` filename at the project root.
pub const FILE_NAME: &str = ".env";

/// Parsed line of a `.env` file. Non-pair lines are preserved verbatim so
/// `set`/`unset` round-trips keep comments and blank lines intact.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Pair { key: String, value: String },
    Other(String),
}

/// Path to the project's `.env` file (regardless of whether it exists).
pub fn env_path(project: &Path) -> PathBuf {
    project.join(FILE_NAME)
}

/// Read every key/value pair currently set in the project's `.env`.
///
/// Returns an empty map when the file does not exist.
pub fn load(project: &Path) -> Result<BTreeMap<String, String>> {
    let lines = read_lines(project)?;
    let mut out = BTreeMap::new();
    for line in lines {
        if let Line::Pair { key, value } = line {
            out.insert(key, value);
        }
    }
    Ok(out)
}

/// Read a single value by key. `None` if the file is missing or the key is
/// absent.
pub fn get(project: &Path, key: &str) -> Result<Option<String>> {
    validate_key(key)?;
    Ok(load(project)?.remove(key))
}

/// Set (or overwrite) a key. Returns `true` if the key already existed.
pub fn set(project: &Path, key: &str, value: &str) -> Result<bool> {
    validate_key(key)?;
    if value.contains('\n') || value.contains('\r') {
        bail!("env values cannot contain newlines");
    }
    let mut lines = read_lines(project)?;
    let mut replaced = false;
    for line in lines.iter_mut() {
        if let Line::Pair {
            key: existing,
            value: existing_value,
        } = line
            && existing == key {
                *existing_value = value.to_string();
                replaced = true;
                break;
            }
    }
    if !replaced {
        lines.push(Line::Pair {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    write_lines(project, &lines)?;
    ensure_gitignored(project)?;
    Ok(replaced)
}

/// Remove a key. Returns `true` if a line was actually removed.
pub fn unset(project: &Path, key: &str) -> Result<bool> {
    validate_key(key)?;
    let lines = read_lines(project)?;
    let before = lines.len();
    let kept: Vec<Line> = lines
        .into_iter()
        .filter(|line| !matches!(line, Line::Pair { key: existing, .. } if existing == key))
        .collect();
    let removed = kept.len() != before;
    if removed {
        write_lines(project, &kept)?;
    }
    Ok(removed)
}

/// Read every key currently set, sorted by key.
pub fn list(project: &Path) -> Result<Vec<(String, String)>> {
    Ok(load(project)?.into_iter().collect())
}

/// Mask a value for display in `env list` / log banners.
pub fn mask(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else if value.len() <= 4 {
        "*".repeat(value.len())
    } else {
        // Show the first two characters as a sanity check ("is that the
        // right token?") and mask the rest.
        let prefix: String = value.chars().take(2).collect();
        format!("{prefix}{}", "*".repeat(value.chars().count() - 2))
    }
}

fn read_lines(project: &Path) -> Result<Vec<Line>> {
    let path = env_path(project);
    let raw = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("read {}", path.display()));
        }
    };
    let mut out = Vec::new();
    for raw_line in raw.split_inclusive('\n') {
        // Strip the trailing newline (if any) for parse logic; we always
        // re-emit `\n` line endings on write.
        let stripped = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        out.push(parse_line(stripped));
    }
    Ok(out)
}

fn parse_line(raw: &str) -> Line {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Line::Other(raw.to_string());
    }
    // Tolerate the leading `export ` that some `.env` flavors emit.
    let body = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let Some((key, value)) = body.split_once('=') else {
        return Line::Other(raw.to_string());
    };
    let key = key.trim();
    if key.is_empty() || !is_valid_key(key) {
        return Line::Other(raw.to_string());
    }
    let value = parse_value(value.trim_end());
    Line::Pair {
        key: key.to_string(),
        value,
    }
}

/// Strip surrounding quotes and decode the small escape set we accept on
/// write. Outside of quoted values, the value is taken verbatim (with
/// surrounding whitespace trimmed off the right by the caller).
fn parse_value(raw: &str) -> String {
    let raw = raw.trim_start();
    if let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return decode_escapes(inner);
    }
    if let Some(inner) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        // Single-quoted: no escape processing.
        return inner.to_string();
    }
    // Unquoted: trim trailing whitespace and accept the rest as-is.
    raw.trim_end().to_string()
}

fn decode_escapes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn write_lines(project: &Path, lines: &[Line]) -> Result<()> {
    let path = env_path(project);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent of {}", path.display()))?;
        }
    let mut buf = String::new();
    for (index, line) in lines.iter().enumerate() {
        match line {
            Line::Pair { key, value } => {
                buf.push_str(key);
                buf.push('=');
                buf.push_str(&encode_value(value));
            }
            Line::Other(raw) => buf.push_str(raw),
        }
        if index + 1 < lines.len() || !buf.ends_with('\n') {
            buf.push('\n');
        }
    }
    fs::write(&path, buf).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn encode_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    // Bare values are fine when every byte is shell-safe and the value
    // does not start with a quote (which would round-trip ambiguously).
    let safe = value
        .chars()
        .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '/' | '.' | ',' | ':' | '@'));
    if safe {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn validate_key(key: &str) -> Result<()> {
    if !is_valid_key(key) {
        bail!("invalid env key `{key}`: must start with [A-Za-z_] and contain only [A-Za-z0-9_]");
    }
    Ok(())
}

/// Append `.env` to the project `.gitignore` if it is not already covered.
/// Idempotent: a second call is a no-op.
fn ensure_gitignored(project: &Path) -> Result<()> {
    let gitignore = project.join(".gitignore");
    let existing = match fs::read_to_string(&gitignore) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            // A read failure here should not break `env set` — surface a
            // warning and continue. The user can always edit .gitignore
            // by hand if their environment is unusual.
            eprintln!("warn: could not read {}: {err}", gitignore.display());
            return Ok(());
        }
    };
    if gitignore_covers_env(&existing) {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore)
        .with_context(|| format!("open {}", gitignore.display()))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.write_all(b"# Added by `pocopine env set`: never commit dev secrets.\n")?;
    file.write_all(b".env\n")?;
    Ok(())
}

fn gitignore_covers_env(contents: &str) -> bool {
    contents.lines().any(|line| {
        let trimmed = line.trim();
        // Patterns that match the project-root `.env` file. We intentionally
        // do not try to interpret `!` re-includes — if the user has crafted
        // a complex ignore stack we trust them and silently skip the
        // auto-append.
        matches!(trimmed, ".env" | "/.env" | "*.env" | ".env*")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn set_writes_new_file_and_round_trips() {
        let dir = tempdir().unwrap();
        assert!(!set(dir.path(), "DATABASE_URL", "postgres://localhost/dev").unwrap());
        assert_eq!(
            get(dir.path(), "DATABASE_URL").unwrap().as_deref(),
            Some("postgres://localhost/dev")
        );
    }

    #[test]
    fn set_replaces_existing_key_in_place_and_preserves_other_lines() {
        let dir = tempdir().unwrap();
        fs::write(
            env_path(dir.path()),
            "# header comment\nKEEP=keep-me\nDATABASE_URL=old\n\n",
        )
        .unwrap();
        assert!(set(dir.path(), "DATABASE_URL", "new").unwrap());
        let text = fs::read_to_string(env_path(dir.path())).unwrap();
        assert!(
            text.contains("# header comment"),
            "comment preserved: {text}"
        );
        assert!(text.contains("KEEP=keep-me"), "other key preserved: {text}");
        assert!(text.contains("DATABASE_URL=new"), "value replaced: {text}");
        assert!(!text.contains("DATABASE_URL=old"), "old value gone: {text}");
    }

    #[test]
    fn quoted_values_round_trip_through_set_and_load() {
        let dir = tempdir().unwrap();
        set(dir.path(), "MOTD", "hello, world!").unwrap();
        set(dir.path(), "MULTI", "a=b c=\"d\"").unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(
            loaded.get("MOTD").map(String::as_str),
            Some("hello, world!")
        );
        assert_eq!(loaded.get("MULTI").map(String::as_str), Some("a=b c=\"d\""));
    }

    #[test]
    fn unset_removes_only_the_named_key() {
        let dir = tempdir().unwrap();
        set(dir.path(), "A", "1").unwrap();
        set(dir.path(), "B", "2").unwrap();
        assert!(unset(dir.path(), "A").unwrap());
        let loaded = load(dir.path()).unwrap();
        assert!(!loaded.contains_key("A"));
        assert_eq!(loaded.get("B").map(String::as_str), Some("2"));
        // Second unset is a no-op.
        assert!(!unset(dir.path(), "A").unwrap());
    }

    #[test]
    fn set_appends_env_to_gitignore_idempotently() {
        let dir = tempdir().unwrap();
        set(dir.path(), "FOO", "bar").unwrap();
        let gi = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(gi.contains(".env"), "{gi}");
        set(dir.path(), "FOO", "baz").unwrap();
        let after = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(
            after.matches(".env").count(),
            gi.matches(".env").count(),
            "no duplicate .env entry on second set"
        );
    }

    #[test]
    fn existing_gitignore_with_env_pattern_is_not_double_appended() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "target/\n.env\n").unwrap();
        set(dir.path(), "FOO", "bar").unwrap();
        let after = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(after.matches(".env").count(), 1, "{after}");
        assert!(!after.contains("Added by `pocopine env set`"));
    }

    #[test]
    fn invalid_keys_are_rejected() {
        let dir = tempdir().unwrap();
        for bad in ["", "1FOO", "FOO BAR", "FOO-BAR", "FOO=BAR", "FOO.BAR"] {
            assert!(
                set(dir.path(), bad, "x").is_err(),
                "should reject key: {bad:?}"
            );
        }
    }

    #[test]
    fn values_with_newlines_are_rejected() {
        let dir = tempdir().unwrap();
        assert!(set(dir.path(), "FOO", "line1\nline2").is_err());
    }

    #[test]
    fn parses_export_prefix_and_single_quotes() {
        let dir = tempdir().unwrap();
        fs::write(
            env_path(dir.path()),
            "export FOO=bar\nQUOTED='literal $var'\n",
        )
        .unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(
            loaded.get("QUOTED").map(String::as_str),
            Some("literal $var")
        );
    }

    #[test]
    fn mask_keeps_short_values_fully_redacted() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("ab"), "**");
        assert_eq!(mask("abcd"), "****");
        assert_eq!(mask("sk_live_1234567890"), "sk****************");
    }
}
