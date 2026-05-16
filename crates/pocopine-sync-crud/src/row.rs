use pocopine_sync::{SyncResult, SyncRow};

use crate::ResourceId;

/// Build an optimistic sync row for a CRUD resource row.
pub fn optimistic_row<Id, Row>(id: &Id, row: Row) -> SyncResult<SyncRow<Row>>
where
    Id: ResourceId,
{
    Ok(SyncRow {
        key: id.to_row_key()?,
        version: None,
        value: row,
        pending: true,
        conflict: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimistic_row_marks_pending() {
        let row = optimistic_row(&"post_1".to_string(), "hello".to_string()).unwrap();

        assert_eq!(row.key.as_str(), "post_1");
        assert!(row.pending);
        assert!(!row.conflict);
        assert_eq!(row.value, "hello");
    }
}
