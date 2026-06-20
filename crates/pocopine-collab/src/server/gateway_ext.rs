//! Correct-by-construction wiring of the collab handler onto a realtime gateway.
//!
//! A [`CollabSync`] handler MUST publish and subscribe through the *same*
//! [`Fanout`](pocopine_realtime::Fanout) instance the gateway uses, or its
//! convergence apply loop folds a fan-out nobody publishes to and multi-process
//! replicas silently diverge. Wiring that by hand is a footgun:
//!
//! ```ignore
//! // easy to get wrong — two different fan-outs, no compile-time link:
//! let gateway = WsGateway::new(fanout.clone())
//!     .with_handler(COLLAB_SUBPROTOCOL, Arc::new(CollabSync::new(other_fanout)));
//! ```
//!
//! These helpers take the handler's fan-out from the gateway itself, so the two
//! provably match:
//!
//! ```ignore
//! use pocopine_collab::WsGatewayCollabExt;
//! let gateway = WsGateway::new(fanout).allow_all_topics().with_collab_store(store);
//! ```

use std::sync::Arc;

use pocopine_realtime::WsGateway;

use super::handler::CollabSync;
use super::store::CollabStore;
use crate::protocol::COLLAB_SUBPROTOCOL;

/// Register a [`CollabSync`] handler under [`COLLAB_SUBPROTOCOL`] using the
/// gateway's own fan-out, so the handler and gateway can never be wired to
/// different instances. Implemented for [`WsGateway`].
pub trait WsGatewayCollabExt {
    /// Wire collaboration with default settings and no durable store
    /// (single-process / dev: state lives only in memory and the fan-out).
    fn with_collab(self) -> Self;

    /// Wire collaboration backed by `store`, so documents survive process
    /// restart and fan-out retention eviction. The canonical production wiring.
    fn with_collab_store(self, store: Arc<dyn CollabStore>) -> Self;

    /// Wire collaboration, customizing the handler via `configure`. The closure
    /// receives a [`CollabSync`] already bound to the gateway's fan-out — chain
    /// [`CollabSync::with_store`], [`CollabSync::with_checkpoint_every`],
    /// [`CollabSync::with_max_update_bytes`] on it. Use this instead of
    /// constructing the handler yourself, which would lose the shared-fan-out
    /// guarantee.
    fn with_collab_configured(self, configure: impl FnOnce(CollabSync) -> CollabSync) -> Self;
}

impl WsGatewayCollabExt for WsGateway {
    fn with_collab(self) -> Self {
        self.with_collab_configured(|handler| handler)
    }

    fn with_collab_store(self, store: Arc<dyn CollabStore>) -> Self {
        self.with_collab_configured(move |handler| handler.with_store(store))
    }

    fn with_collab_configured(self, configure: impl FnOnce(CollabSync) -> CollabSync) -> Self {
        // The handler folds the gateway's OWN fan-out: this is the single line
        // that makes the shared-instance requirement impossible to get wrong.
        let handler = configure(CollabSync::new(self.fanout().clone()));
        self.with_handler(COLLAB_SUBPROTOCOL, Arc::new(handler))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::store::MemoryCollabStore;

    #[test]
    fn helpers_build_a_gateway_for_each_variant() {
        // The wiring is correct-by-construction (the handler takes the gateway's
        // own fan-out); here we just prove every entry point composes into a
        // usable gateway. End-to-end convergence through the helper is covered by
        // the `two_replicas_*` integration test in `tests/gateway.rs`.
        let _ = WsGateway::local().allow_all_topics().with_collab();

        let store: Arc<dyn CollabStore> = Arc::new(MemoryCollabStore::new());
        let _ = WsGateway::local()
            .allow_all_topics()
            .with_collab_store(store.clone());

        let _ = WsGateway::local()
            .allow_all_topics()
            .with_collab_configured(move |h| h.with_store(store).with_checkpoint_every(8));
    }
}
