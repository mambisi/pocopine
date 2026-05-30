use pocopine_crypto::{digest_hex, Algorithm, Hasher};

use crate::{ChecksumAlgorithm, ChecksumPolicy, ObjectChecksum, StorageError, StorageResult};

/// Incremental hasher for streaming upload bytes.
///
/// Native multipart/resumable backends never hold a whole object in memory, so
/// they cannot call [`validate_complete_checksum`] (which takes the full
/// buffer). They stream the stored object through this hasher and validate the
/// result with [`validate_complete_checksum_precomputed`].
pub struct StreamingChecksum {
    algorithm: ChecksumAlgorithm,
    hasher: Hasher,
}

impl StreamingChecksum {
    pub fn new(algorithm: ChecksumAlgorithm) -> Self {
        Self {
            algorithm,
            hasher: Hasher::new(crypto_algorithm(algorithm)),
        }
    }

    pub fn update(&mut self, chunk: &[u8]) {
        self.hasher.update(chunk);
    }

    pub fn finalize(self) -> ObjectChecksum {
        ObjectChecksum {
            algorithm: self.algorithm,
            value: self.hasher.finalize_hex(),
        }
    }
}

/// The checksum algorithm a completed upload must hash, if any.
///
/// Returns `None` when no verification is needed (no policy, or an `Optional`
/// policy with no client-supplied checksum), so a streaming backend can skip
/// hashing entirely in the common case. Backends should stream-compute this
/// algorithm and pass the result to [`validate_complete_checksum_precomputed`].
pub fn checksum_algorithm_to_compute(
    policy: &ChecksumPolicy,
    provided: Option<&ObjectChecksum>,
) -> Option<ChecksumAlgorithm> {
    match policy {
        ChecksumPolicy::None => None,
        ChecksumPolicy::Optional(_) => provided.map(|checksum| checksum.algorithm),
        ChecksumPolicy::Required(algorithm) => Some(*algorithm),
    }
}

/// Validate a `provided` checksum against one the backend already computed while
/// streaming, instead of re-hashing a full in-memory buffer.
///
/// `computed` is the checksum the backend streamed for the stored object (or
/// `None` when [`checksum_algorithm_to_compute`] returned `None`).
pub fn validate_complete_checksum_precomputed(
    policy: &ChecksumPolicy,
    computed: Option<ObjectChecksum>,
    provided: Option<ObjectChecksum>,
) -> StorageResult<Option<ObjectChecksum>> {
    ensure_supported_checksum_policy(policy)?;
    match policy {
        ChecksumPolicy::None => Ok(None),
        ChecksumPolicy::Optional(allowed) => match provided {
            Some(checksum) => {
                if !allowed.contains(&checksum.algorithm) {
                    return Err(StorageError::policy_rejected(
                        "checksum algorithm is not allowed",
                    ));
                }
                verify_match(&checksum, expect_computed(checksum.algorithm, computed)?)
            }
            None => Ok(None),
        },
        ChecksumPolicy::Required(algorithm) => {
            let provided = provided.ok_or_else(|| {
                StorageError::policy_rejected("required upload checksum is missing")
            })?;
            if provided.algorithm != *algorithm {
                return Err(StorageError::policy_rejected(
                    "required upload checksum algorithm does not match policy",
                ));
            }
            verify_match(&provided, expect_computed(*algorithm, computed)?)
        }
    }
}

pub fn ensure_supported_checksum_policy(policy: &ChecksumPolicy) -> StorageResult<()> {
    // Every `ChecksumAlgorithm` variant is now backed by `pocopine-crypto`, so
    // there is nothing left to reject. Kept as a public hook so backends keep a
    // single validation entry point if future algorithms are added.
    let _ = policy;
    Ok(())
}

pub fn validate_complete_checksum(
    policy: &ChecksumPolicy,
    bytes: &[u8],
    provided: Option<ObjectChecksum>,
) -> StorageResult<Option<ObjectChecksum>> {
    match policy {
        ChecksumPolicy::None => Ok(None),
        ChecksumPolicy::Optional(allowed) => match provided {
            Some(checksum) => {
                if !allowed.contains(&checksum.algorithm) {
                    return Err(StorageError::policy_rejected(
                        "checksum algorithm is not allowed",
                    ));
                }
                verify_match(&checksum, compute_checksum(checksum.algorithm, bytes))
            }
            None => Ok(None),
        },
        ChecksumPolicy::Required(algorithm) => {
            let provided = provided.ok_or_else(|| {
                StorageError::policy_rejected("required upload checksum is missing")
            })?;
            if provided.algorithm != *algorithm {
                return Err(StorageError::policy_rejected(
                    "required upload checksum algorithm does not match policy",
                ));
            }
            verify_match(&provided, compute_checksum(*algorithm, bytes))
        }
    }
}

/// Confirm a `computed` checksum matches what the client `provided`.
fn verify_match(
    provided: &ObjectChecksum,
    computed: ObjectChecksum,
) -> StorageResult<Option<ObjectChecksum>> {
    if !provided.value.eq_ignore_ascii_case(&computed.value) {
        return Err(StorageError::policy_rejected(
            "upload checksum does not match uploaded bytes",
        ));
    }
    Ok(Some(computed))
}

fn expect_computed(
    algorithm: ChecksumAlgorithm,
    computed: Option<ObjectChecksum>,
) -> StorageResult<ObjectChecksum> {
    let computed = computed.ok_or_else(|| {
        StorageError::backend("checksum was not computed for the completed object".to_string())
    })?;
    if computed.algorithm != algorithm {
        return Err(StorageError::backend(
            "computed checksum algorithm does not match the requested algorithm".to_string(),
        ));
    }
    Ok(computed)
}

fn compute_checksum(algorithm: ChecksumAlgorithm, bytes: &[u8]) -> ObjectChecksum {
    ObjectChecksum {
        algorithm,
        value: digest_hex(crypto_algorithm(algorithm), bytes),
    }
}

fn crypto_algorithm(algorithm: ChecksumAlgorithm) -> Algorithm {
    match algorithm {
        ChecksumAlgorithm::Sha256 => Algorithm::Sha256,
        ChecksumAlgorithm::Crc32c => Algorithm::Crc32c,
        ChecksumAlgorithm::Md5 => Algorithm::Md5,
    }
}
