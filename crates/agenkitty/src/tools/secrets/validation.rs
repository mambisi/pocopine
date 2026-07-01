use std::collections::{BTreeMap, HashSet};

use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

/// Validate a model-requested environment variable name used as a destination
/// for a secret handle. Dynamic-loader variables and `PATH` can subvert command
/// execution, so they are never valid secret env destinations.
pub fn validate_secret_env_name(name: &str) -> AgenkitResult<()> {
    if name.is_empty()
        || name == "PATH"
        || name.starts_with("LD_")
        || name.starts_with("DYLD_")
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
        || name.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return Err(AgenkitError::tool_policy(format!(
            "invalid secret env destination `{name}`"
        )));
    }
    Ok(())
}

pub fn resolve_secret_headers(
    headers: &BTreeMap<String, String>,
) -> AgenkitResult<Vec<(reqwest::header::HeaderName, String)>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (name, handle) in headers {
        let header = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
            AgenkitError::validation(format!("invalid secret header `{name}`: {err}"))
        })?;
        if !seen.insert(header.clone()) {
            return Err(AgenkitError::validation(format!(
                "duplicate secret header `{name}`"
            )));
        }
        out.push((header, handle.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_env_names_reject_execution_control_vars() {
        assert!(validate_secret_env_name("API_TOKEN").is_ok());
        assert!(validate_secret_env_name("PATH").is_err());
        assert!(validate_secret_env_name("LD_PRELOAD").is_err());
        assert!(validate_secret_env_name("lowercase").is_err());
        assert!(validate_secret_env_name("1TOKEN").is_err());
    }
}
