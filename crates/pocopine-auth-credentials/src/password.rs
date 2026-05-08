//! Argon2id wrapper.
//!
//! All hashing and verifying runs through `tokio::task::spawn_blocking`
//! so an argon2id call (~60–250 ms CPU at OWASP defaults) doesn't
//! block the runtime. The crate re-exports
//! [`Argon2Params::owasp_default`] /
//! [`Argon2Params::owasp_minimum`] so apps can dial defaults
//! deliberately rather than guess.

use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};

use crate::error::CredentialsError;

/// Configurable argon2id parameters.
///
/// `m_cost_kib` is memory cost in KiB. OWASP's
/// [Password Storage Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html#argon2id)
/// recommends m=64 MiB / t=3 / p=4 as the modern default for argon2id;
/// the documented absolute minimum is m=19 MiB / t=2 / p=1.
/// [`Self::validate`] enforces the minimum.
#[derive(Clone, Debug)]
pub struct Argon2Params {
    /// Memory cost, KiB.
    pub m_cost_kib: u32,
    /// Iteration count.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
    /// Output length, bytes.
    pub output_len: usize,
}

/// Failure modes for [`Argon2Params::validate`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Argon2ParamsError {
    BelowMinMemory,
    BelowMinTime,
    BelowMinParallelism,
    InvalidOutputLength,
}

impl std::fmt::Display for Argon2ParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BelowMinMemory => f.write_str("argon2 m_cost_kib below OWASP minimum (19456)"),
            Self::BelowMinTime => f.write_str("argon2 t_cost below OWASP minimum (2)"),
            Self::BelowMinParallelism => f.write_str("argon2 p_cost below OWASP minimum (1)"),
            Self::InvalidOutputLength => {
                f.write_str("argon2 output_len out of range (must be 16..=64)")
            }
        }
    }
}

impl std::error::Error for Argon2ParamsError {}

impl Argon2Params {
    /// OWASP-recommended modern default for argon2id: m=64 MiB,
    /// t=3, p=4, 32-byte output.
    pub const fn owasp_default() -> Self {
        Self {
            m_cost_kib: 65_536,
            t_cost: 3,
            p_cost: 4,
            output_len: 32,
        }
    }

    /// OWASP minimum for argon2id: m=19 MiB, t=2, p=1.
    pub const fn owasp_minimum() -> Self {
        Self {
            m_cost_kib: 19_456,
            t_cost: 2,
            p_cost: 1,
            output_len: 32,
        }
    }

    /// Reject parameters weaker than the OWASP minimum.
    pub fn validate(&self) -> Result<(), Argon2ParamsError> {
        let min = Self::owasp_minimum();
        if self.m_cost_kib < min.m_cost_kib {
            return Err(Argon2ParamsError::BelowMinMemory);
        }
        if self.t_cost < min.t_cost {
            return Err(Argon2ParamsError::BelowMinTime);
        }
        if self.p_cost < min.p_cost {
            return Err(Argon2ParamsError::BelowMinParallelism);
        }
        if !(16..=64).contains(&self.output_len) {
            return Err(Argon2ParamsError::InvalidOutputLength);
        }
        Ok(())
    }

    fn to_inner(&self) -> Result<Params, CredentialsError> {
        Params::new(self.m_cost_kib, self.t_cost, self.p_cost, Some(self.output_len))
            .map_err(|_| CredentialsError::PasswordHashing("invalid argon2 params"))
    }
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::owasp_default()
    }
}

/// Hash `password` to a fresh argon2id PHC string. Runs the CPU
/// work on a blocking thread so the async runtime stays responsive.
pub async fn hash_password(
    password: String,
    params: Argon2Params,
) -> Result<String, CredentialsError> {
    tokio::task::spawn_blocking(move || hash_blocking(&password, &params))
        .await
        .map_err(|_| CredentialsError::PasswordHashing("argon2 task panicked"))?
}

/// Verify `password` against the stored PHC string. Constant-time
/// at the argon2 level.
pub async fn verify_password(
    password: String,
    hash: String,
) -> Result<bool, CredentialsError> {
    tokio::task::spawn_blocking(move || verify_blocking(&password, &hash))
        .await
        .map_err(|_| CredentialsError::PasswordHashing("argon2 task panicked"))?
}

/// Synchronous argon2id hash. Used at builder-install time to
/// pre-bake the constant-time-login dummy hash without entering a
/// nested tokio runtime. Production hashing for signup goes through
/// [`hash_password`] (which wraps this in `spawn_blocking`).
pub(crate) fn hash_password_sync(
    password: &str,
    params: &Argon2Params,
) -> Result<String, CredentialsError> {
    hash_blocking(password, params)
}

fn hash_blocking(password: &str, params: &Argon2Params) -> Result<String, CredentialsError> {
    let inner = params.to_inner()?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, inner);
    // UUIDv4's 16 random bytes are well above argon2's 8-byte
    // minimum salt length and don't require an extra `rand` /
    // `rand_core` dep just for salt generation. The OS RNG drives
    // UUIDv4 under the hood, so this is the same entropy source
    // `SaltString::generate(&mut OsRng)` would use.
    let salt_uuid = uuid::Uuid::new_v4();
    let salt = SaltString::encode_b64(salt_uuid.as_bytes())
        .map_err(|_| CredentialsError::PasswordHashing("salt encode failed"))?;
    argon
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| CredentialsError::PasswordHashing("argon2 hash failed"))
}

fn verify_blocking(password: &str, hash: &str) -> Result<bool, CredentialsError> {
    let parsed = PasswordHash::new(hash)
        .map_err(|_| CredentialsError::PasswordHashing("phc string malformed"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_and_verify_round_trip() {
        let params = Argon2Params::owasp_minimum();
        let hash = hash_password("hunter2hunter2".to_string(), params.clone())
            .await
            .unwrap();
        assert!(verify_password("hunter2hunter2".to_string(), hash.clone())
            .await
            .unwrap());
        assert!(!verify_password("wrong-password".to_string(), hash)
            .await
            .unwrap());
    }

    #[test]
    fn validate_rejects_below_minimum() {
        let mut weak = Argon2Params::owasp_minimum();
        weak.m_cost_kib = 1024;
        assert!(matches!(
            weak.validate(),
            Err(Argon2ParamsError::BelowMinMemory)
        ));

        let mut weak = Argon2Params::owasp_minimum();
        weak.t_cost = 0;
        assert!(matches!(
            weak.validate(),
            Err(Argon2ParamsError::BelowMinTime)
        ));

        let mut weak = Argon2Params::owasp_minimum();
        weak.p_cost = 0;
        assert!(matches!(
            weak.validate(),
            Err(Argon2ParamsError::BelowMinParallelism)
        ));

        let mut weak = Argon2Params::owasp_minimum();
        weak.output_len = 8;
        assert!(matches!(
            weak.validate(),
            Err(Argon2ParamsError::InvalidOutputLength)
        ));
    }

    #[test]
    fn defaults_pass_validation() {
        Argon2Params::owasp_default().validate().unwrap();
        Argon2Params::owasp_minimum().validate().unwrap();
    }
}
