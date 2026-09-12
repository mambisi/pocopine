use std::collections::LinkedList;
use std::mem::size_of;

use crate::{ChangeSet, CodeError, CodeResult, EditOrigin, Selection};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryLimits {
    pub max_groups: usize,
    pub max_bytes: usize,
    pub group_delay_ms: u64,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_groups: 200,
            max_bytes: 8 * 1024 * 1024,
            group_delay_ms: 500,
        }
    }
}

impl HistoryLimits {
    pub fn validate(self) -> CodeResult<()> {
        if self.max_groups == 0 || self.max_bytes == 0 {
            Err(CodeError::InvalidConfiguration)
        } else {
            Ok(())
        }
    }
}

pub(crate) struct HistoryEntry {
    pub forward: ChangeSet,
    pub backward: ChangeSet,
    pub before: Selection,
    pub after: Selection,
}

pub(crate) struct Group {
    pub entries: Vec<HistoryEntry>,
    last_time: u64,
    kind: Option<InputKind>,
}

impl Group {
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>() // linked-list ownership, no retained spare slots
            + self.entries.capacity() * size_of::<HistoryEntry>()
            + self
                .entries
                .iter()
                .map(|entry| entry.forward.retained_bytes() + entry.backward.retained_bytes())
                .sum::<usize>()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Insert,
    Backspace,
    Delete,
}

fn kind(entry: &HistoryEntry, origin: EditOrigin) -> Option<InputKind> {
    if origin != EditOrigin::Input || !entry.before.is_empty() || !entry.after.is_empty() {
        return None;
    }
    let [change] = entry.forward.changes() else {
        return None;
    };
    if change.range.is_empty()
        && change.range.from == entry.before.head
        && entry.after.head.0 == change.range.from.0 + change.insert.len()
        && !change.insert.contains('\n')
    {
        Some(InputKind::Insert)
    } else if change.insert.is_empty()
        && change.range.to == entry.before.head
        && change.range.from == entry.after.head
    {
        Some(InputKind::Backspace)
    } else if change.insert.is_empty()
        && change.range.from == entry.before.head
        && entry.before == entry.after
    {
        Some(InputKind::Delete)
    } else {
        None
    }
}

pub(crate) struct History {
    undo: LinkedList<Group>,
    redo: LinkedList<Group>,
    limits: HistoryLimits,
    open: bool,
}

impl History {
    pub fn new(limits: HistoryLimits) -> CodeResult<Self> {
        limits.validate()?;
        Ok(Self {
            undo: LinkedList::new(),
            redo: LinkedList::new(),
            limits,
            open: false,
        })
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = false;
    }
    pub fn close_group(&mut self) {
        self.open = false;
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(Group::bytes)
            .sum()
    }
    pub fn peek(&self, redo: bool) -> Option<&Group> {
        if redo {
            self.redo.back()
        } else {
            self.undo.back()
        }
    }

    pub fn record(&mut self, entry: HistoryEntry, origin: EditOrigin, time: u64) -> bool {
        self.redo.clear();
        let kind = kind(&entry, origin);
        let joins = self.open
            && kind.is_some()
            && self.undo.back().is_some_and(|group| {
                group.kind == kind
                    && time >= group.last_time
                    && time - group.last_time <= self.limits.group_delay_ms
                    && group
                        .entries
                        .last()
                        .is_some_and(|last| last.after == entry.before)
            });
        if joins {
            let group = self.undo.back_mut().unwrap();
            group.entries.push(entry);
            group.last_time = time;
        } else {
            self.undo.push_back(Group {
                entries: vec![entry],
                last_time: time,
                kind,
            });
        }
        self.open = kind.is_some();
        while self.undo.len() > self.limits.max_groups
            || self.undo.iter().map(Group::bytes).sum::<usize>() > self.limits.max_bytes
        {
            self.undo.pop_front();
        }
        if self.undo.is_empty() {
            self.open = false;
        }
        !self.undo.is_empty()
    }

    pub fn travel(&mut self, redo: bool) {
        self.open = false;
        if redo {
            if let Some(group) = self.redo.pop_back() {
                self.undo.push_back(group);
            }
        } else if let Some(group) = self.undo.pop_back() {
            self.redo.push_back(group);
        }
        // Travel transfers ownership without cloning; the combined retention
        // budget is unchanged. A new edit discards redo before enforcing limits.
    }
}
