use std::{fmt, hash::Hash, str::FromStr};

use pocopine_sync::{RowKey, SyncError, SyncResult};
use serde::{de::DeserializeOwned, Serialize};

/// Resource identity boundary for sync CRUD resources.
///
/// Implementations encode themselves through [`Display`](fmt::Display) and
/// decode through [`FromStr`]. This keeps common Rust id types usable without
/// wrapper-specific conversion methods.
pub trait ResourceId:
    Clone + Eq + Hash + fmt::Display + FromStr + Serialize + DeserializeOwned + Send + Sync + 'static
{
    /// Generate a local-first id when this type supports client-side ids.
    fn generate_local() -> SyncResult<Self> {
        Err(SyncError::unsupported(
            "this resource id does not support local generation",
        ))
    }

    /// Encode this id as a sync row key.
    fn to_row_key(&self) -> SyncResult<RowKey> {
        RowKey::new(self.to_string())
    }

    /// Decode this id from a sync row key.
    fn from_row_key(row_key: &RowKey) -> SyncResult<Self> {
        Self::from_str(row_key.as_str())
            .map_err(|_| SyncError::client(format!("invalid resource id: {}", row_key.as_str())))
    }
}

impl ResourceId for String {
    fn to_row_key(&self) -> SyncResult<RowKey> {
        RowKey::new(self.clone())
    }
}

macro_rules! integer_resource_id {
    ($($ty:ty),* $(,)?) => {
        $(
            impl ResourceId for $ty {}
        )*
    };
}

integer_resource_id!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl ResourceId for uuid::Uuid {
    fn generate_local() -> SyncResult<Self> {
        Ok(uuid::Uuid::new_v4())
    }
}

/// Generate a local-first id for a resource id type.
///
/// This is the function generated resource modules should call from their
/// `new_id()` helper. Types that cannot safely allocate client-side ids return
/// [`SyncError::Unsupported`](pocopine_sync::SyncError::Unsupported).
pub fn new_id<Id>() -> SyncResult<Id>
where
    Id: ResourceId,
{
    Id::generate_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_id_converts_to_and_from_row_key() {
        let id = 42_i64;
        let row_key = id.to_row_key().unwrap();

        assert_eq!(row_key.as_str(), "42");
        assert_eq!(i64::from_row_key(&row_key).unwrap(), id);
    }

    #[test]
    fn string_resource_id_uses_validated_row_key() {
        assert!("customer_1".to_string().to_row_key().is_ok());
        assert!("bad\nid".to_string().to_row_key().is_err());
    }

    #[test]
    fn uuid_resource_id_can_generate_local_ids() {
        let id = new_id::<uuid::Uuid>().unwrap();
        let row_key = id.to_row_key().unwrap();

        assert_eq!(uuid::Uuid::from_row_key(&row_key).unwrap(), id);
    }

    #[test]
    fn integer_resource_ids_do_not_generate_local_ids_by_default() {
        let err = new_id::<i64>().unwrap_err();

        assert!(err
            .to_string()
            .contains("does not support local generation"));
    }
}
