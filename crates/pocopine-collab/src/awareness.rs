//! Ephemeral presence (cursors, selections, identity) over the collab gateway.
//!
//! A thin wrapper over [`yrs::sync::Awareness`] that produces and consumes the
//! body of a [`CollabMessage::Awareness`](crate::CollabMessage::Awareness) frame.
//! The server relays those frames verbatim — never applied to the document,
//! never persisted (see `server::handler`) — and each client folds peers'
//! updates into its own awareness to render their presence.
//!
//! Presence state is application-defined: a canvas publishes a pointer position,
//! a rich-text editor publishes a cursor/selection. Anything `serde`-serializable
//! works; this module is schema-agnostic, exactly like the rest of the transport.

use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;
use yrs::sync::time::{Clock, Timestamp};
use yrs::sync::{Awareness, AwarenessUpdate};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{ClientID, Doc};

use crate::error::{CollabError, CollabResult};

/// A strictly-increasing per-instance clock for awareness.
///
/// yrs' default `SystemClock` is host-only (no wall clock on `wasm32-unknown`),
/// and we don't need real time: yrs compares each client's awareness clock only
/// against that *same* client's previous value, and every tab gets a fresh yrs
/// client id per session — so a counter starting at zero is sufficient, and
/// `fetch_add` guarantees strict monotonicity (no same-millisecond ties).
#[derive(Default)]
struct MonotonicClock(AtomicU64);

impl Clock for MonotonicClock {
    fn now(&self) -> Timestamp {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Per-connection presence for one collab document: this client's own state plus
/// every peer's last-seen state, with yrs awareness clocks deduping out-of-order
/// updates and signalling removals.
pub struct Presence {
    awareness: Awareness,
}

impl Presence {
    /// Bind presence to `doc` — pass the same yrs [`Doc`] the document syncs on,
    /// so the presence client id matches the document's (one id per tab).
    pub fn new(doc: Doc) -> Self {
        Self {
            awareness: Awareness::with_clock(doc, MonotonicClock::default()),
        }
    }

    /// This client's id (the document's yrs client id).
    pub fn client_id(&self) -> u64 {
        self.awareness.client_id().get()
    }

    /// Publish this client's local presence `state`, returning the encoded update
    /// to broadcast as a [`CollabMessage::Awareness`](crate::CollabMessage::Awareness)
    /// body. Only the *local* client's entry is encoded, so a cursor move doesn't
    /// re-broadcast every peer.
    pub fn announce<S: Serialize>(&mut self, state: S) -> CollabResult<Bytes> {
        self.awareness
            .set_local_state(state)
            .map_err(|err| CollabError::Apply(err.to_string()))?;
        self.local_update()
    }

    /// Drop this client's local presence (e.g. on disconnect / page unload),
    /// returning the removal update to broadcast so peers can clear our cursor.
    pub fn clear_local(&mut self) -> CollabResult<Bytes> {
        self.awareness.clean_local_state();
        self.local_update()
    }

    /// Apply a peer's awareness update (a relayed `Awareness` body), returning the
    /// client ids whose presence changed (added / updated / removed), so the
    /// caller can re-render just those cursors.
    pub fn apply(&mut self, body: &[u8]) -> CollabResult<Vec<u64>> {
        let update =
            AwarenessUpdate::decode_v1(body).map_err(|err| CollabError::Decode(err.to_string()))?;
        let summary = self
            .awareness
            .apply_update_summary(update)
            .map_err(|err| CollabError::Apply(err.to_string()))?;
        Ok(summary
            .map(|s| s.all_changes().into_iter().map(|id| id.get()).collect())
            .unwrap_or_default())
    }

    /// Decode a single client's presence state as `S`, or `None` if that client is
    /// unknown / its state doesn't deserialize.
    pub fn state<S: DeserializeOwned>(&self, client_id: u64) -> Option<S> {
        self.awareness.state::<S>(ClientID::new(client_id))
    }

    /// Every known client's presence decoded as `S`, **including this client** —
    /// filter on [`Self::client_id`] to drop self when rendering peers.
    pub fn states<S: DeserializeOwned>(&self) -> Vec<(u64, S)> {
        let ids: Vec<u64> = self.awareness.iter().map(|(id, _)| id.get()).collect();
        ids.into_iter()
            .filter_map(|id| self.state::<S>(id).map(|s| (id, s)))
            .collect()
    }

    /// Encode just the local client's current entry (a fresh state or a removal).
    fn local_update(&self) -> CollabResult<Bytes> {
        let update = self
            .awareness
            .update_with_clients([self.awareness.client_id()])
            .map_err(|err| CollabError::Apply(err.to_string()))?;
        Ok(Bytes::from(update.encode_v1()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    struct Cursor {
        x: f64,
        y: f64,
    }

    fn presence(client_id: u64) -> Presence {
        Presence::new(Doc::with_client_id(client_id))
    }

    #[test]
    fn announce_then_apply_converges_peer_state() {
        let mut a = presence(1);
        let mut b = presence(2);

        let update = a.announce(Cursor { x: 10.0, y: 20.0 }).unwrap();
        let changed = b.apply(&update).unwrap();
        assert_eq!(changed, vec![1]);

        // B now sees A's cursor under A's client id.
        assert_eq!(b.state::<Cursor>(1), Some(Cursor { x: 10.0, y: 20.0 }));
        // ...and A's own update doesn't fabricate a B entry.
        assert_eq!(b.state::<Cursor>(2), None);
    }

    #[test]
    fn clear_local_removes_the_peer() {
        let mut a = presence(1);
        let mut b = presence(2);

        b.apply(&a.announce(Cursor { x: 1.0, y: 2.0 }).unwrap())
            .unwrap();
        assert!(b.state::<Cursor>(1).is_some());

        let removal = a.clear_local().unwrap();
        let changed = b.apply(&removal).unwrap();
        assert_eq!(changed, vec![1]);
        assert_eq!(b.state::<Cursor>(1), None);
    }

    #[test]
    fn states_lists_every_known_client() {
        let mut a = presence(1);
        let mut b = presence(2);
        b.apply(&a.announce(Cursor { x: 5.0, y: 5.0 }).unwrap())
            .unwrap();
        // B knows itself only after it announces.
        b.announce(Cursor { x: 9.0, y: 9.0 }).unwrap();
        let mut ids: Vec<u64> = b.states::<Cursor>().into_iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }
}
