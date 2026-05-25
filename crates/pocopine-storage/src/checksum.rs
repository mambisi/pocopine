use sha2::{Digest, Sha256};

use crate::{ChecksumAlgorithm, ObjectChecksum, StorageError, StorageResult};

pub(crate) fn validate_complete_checksum(
    policy: &crate::ChecksumPolicy,
    bytes: &[u8],
    provided: Option<ObjectChecksum>,
) -> StorageResult<Option<ObjectChecksum>> {
    match policy {
        crate::ChecksumPolicy::None => Ok(None),
        crate::ChecksumPolicy::Optional(allowed) => {
            if let Some(checksum) = &provided {
                if !allowed.iter().any(|allowed| *allowed == checksum.algorithm) {
                    return Err(StorageError::policy_rejected(
                        "checksum algorithm is not allowed",
                    ));
                }
                let computed = compute_checksum(checksum.algorithm, bytes)?;
                if !checksum.value.eq_ignore_ascii_case(&computed.value) {
                    return Err(StorageError::policy_rejected(
                        "upload checksum does not match uploaded bytes",
                    ));
                }
                return Ok(Some(computed));
            }
            Ok(None)
        }
        crate::ChecksumPolicy::Required(algorithm) => {
            let provided = provided.ok_or_else(|| {
                StorageError::policy_rejected("required upload checksum is missing")
            })?;
            if provided.algorithm != *algorithm {
                return Err(StorageError::policy_rejected(
                    "required upload checksum algorithm does not match policy",
                ));
            }
            let computed = compute_checksum(*algorithm, bytes)?;
            if !provided.value.eq_ignore_ascii_case(&computed.value) {
                return Err(StorageError::policy_rejected(
                    "required upload checksum does not match uploaded bytes",
                ));
            }
            Ok(Some(computed))
        }
    }
}

fn compute_checksum(algorithm: ChecksumAlgorithm, bytes: &[u8]) -> StorageResult<ObjectChecksum> {
    match algorithm {
        ChecksumAlgorithm::Sha256 => {
            let digest = Sha256::digest(bytes);
            let mut value = String::with_capacity(digest.len() * 2);
            for byte in digest {
                use std::fmt::Write as _;
                write!(&mut value, "{byte:02x}")
                    .map_err(|err| StorageError::backend(format!("format checksum: {err}")))?;
            }
            Ok(ObjectChecksum { algorithm, value })
        }
        ChecksumAlgorithm::Crc32c | ChecksumAlgorithm::Md5 => Err(StorageError::unsupported(
            "only sha256 checksum verification is implemented in pocopine-storage PR 1",
        )),
    }
}
