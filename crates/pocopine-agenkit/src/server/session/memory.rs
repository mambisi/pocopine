//! In-memory [`SessionStore`] — for tests, examples, and single-process dev.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use super::{
    ParentLink, Record, RecordKind, SessionError, SessionFuture, SessionStore, ThreadId,
    ThreadMeta, now_ms,
};

struct Entry {
    meta: ThreadMeta,
    records: Vec<Record>,
}

/// A `Mutex<HashMap>`-backed session store. No durability — state is lost on
/// drop. Use [`JsonlSessionStore`](super::JsonlSessionStore) for persistence.
#[derive(Default)]
pub struct MemorySessionStore {
    threads: Mutex<HashMap<ThreadId, Entry>>,
}

impl MemorySessionStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the thread map, mapping a poisoned lock to a backend error.
    fn lock(&self) -> super::SessionResult<MutexGuard<'_, HashMap<ThreadId, Entry>>> {
        self.threads
            .lock()
            .map_err(|_| SessionError::Backend("memory session store lock poisoned".into()))
    }
}

/// Box a synchronously-computed result as a ready future (the in-memory store
/// never awaits — it locks, works, and returns).
fn ready<'a, T: Send + 'a>(value: super::SessionResult<T>) -> SessionFuture<'a, T> {
    Box::pin(async move { value })
}

impl SessionStore for MemorySessionStore {
    fn create_thread<'a>(
        &'a self,
        parent: Option<ParentLink>,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ThreadMeta> {
        ready((|| {
            let id = ThreadId::mint()?;
            let meta = ThreadMeta {
                id: id.clone(),
                parent,
                created_ms: now_ms(),
                last_seq: 0,
                attributes,
            };
            let mut threads = self.lock()?;
            threads.insert(
                id,
                Entry {
                    meta: meta.clone(),
                    records: Vec::new(),
                },
            );
            Ok(meta)
        })())
    }

    fn meta<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Option<ThreadMeta>> {
        ready((|| {
            let threads = self.lock()?;
            Ok(threads.get(id).map(|e| e.meta.clone()))
        })())
    }

    fn threads<'a>(&'a self) -> SessionFuture<'a, Vec<ThreadMeta>> {
        ready((|| {
            let threads = self.lock()?;
            Ok(threads.values().map(|e| e.meta.clone()).collect())
        })())
    }

    fn set_attributes<'a>(
        &'a self,
        id: &'a ThreadId,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ()> {
        ready((|| {
            let mut threads = self.lock()?;
            let entry = threads.get_mut(id).ok_or(SessionError::NotFound)?;
            entry.meta.attributes = attributes;
            Ok(())
        })())
    }

    fn append<'a>(
        &'a self,
        id: &'a ThreadId,
        kind: RecordKind,
        data: serde_json::Value,
    ) -> SessionFuture<'a, Record> {
        ready((|| {
            let mut threads = self.lock()?;
            let entry = threads.get_mut(id).ok_or(SessionError::NotFound)?;
            let record = Record {
                seq: entry.records.len() as u64,
                timestamp_ms: now_ms(),
                kind,
                data,
            };
            entry.records.push(record.clone());
            entry.meta.last_seq = entry.records.len() as u64;
            Ok(record)
        })())
    }

    fn records<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<Record>> {
        ready((|| {
            let threads = self.lock()?;
            let entry = threads.get(id).ok_or(SessionError::NotFound)?;
            Ok(entry.records.clone())
        })())
    }

    fn children<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<ThreadId>> {
        ready((|| {
            let threads = self.lock()?;
            Ok(threads
                .values()
                .filter(|e| e.meta.parent.as_ref().is_some_and(|p| &p.parent == id))
                .map(|e| e.meta.id.clone())
                .collect())
        })())
    }

    fn delete_thread<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, ()> {
        ready((|| {
            let mut threads = self.lock()?;
            // Refuse if a fork still points here — deleting would orphan it.
            if threads
                .values()
                .any(|e| e.meta.parent.as_ref().is_some_and(|p| &p.parent == id))
            {
                return Err(SessionError::HasChildren);
            }
            threads.remove(id);
            Ok(())
        })())
    }
}
