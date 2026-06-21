use crate::SyncOp;

pub(super) struct LocalChanges {
    pub(super) had_reset: bool,
    pub(super) items: Vec<crate::SyncChange<serde_json::Value>>,
}

pub(super) fn changes_after_last_reset(
    changes: Vec<crate::SyncChange<serde_json::Value>>,
) -> LocalChanges {
    let Some(index) = changes
        .iter()
        .rposition(|change| change.op == SyncOp::Reset)
    else {
        return LocalChanges {
            had_reset: false,
            items: changes,
        };
    };

    LocalChanges {
        had_reset: true,
        items: changes.into_iter().skip(index).collect(),
    }
}
