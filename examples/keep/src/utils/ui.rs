use crate::KeepStore;

/// Snapshot the document layout, run `action` against the
/// `KeepStore` singleton, then before the very next browser paint
/// replay the projection. Every data-pp-layout-id element that
/// exists in both the before and after states animates from its
/// old rect to its new one — card → editor on open, editor →
/// card on close.
///
/// Scheduled with `tick::next_frame` (requestAnimationFrame), not
/// `after_flush` (setTimeout 0), so the projection's first
/// transform commits in the same paint as the DOM mutation —
/// otherwise there's a one-frame flicker where the source card
/// appears at its rest position before the WAAPI animation has a
/// chance to apply its from-keyframe.
///
/// Wasm only because pine-motion goes through web_sys / DomRect;
/// host build just dispatches the action.
pub fn shared_layout_transition(action: impl FnOnce(&mut KeepStore) + 'static) {
    #[cfg(target_arch = "wasm32")]
    let snapshot = pocopine::dom::document_element()
        .map(|root| (root.clone(), pine_motion::snapshot_layout(&root)));

    pocopine::store::<KeepStore>().update(action);

    #[cfg(target_arch = "wasm32")]
    if let Some((root, snap)) = snapshot {
        pocopine::tick::next_frame(move || {
            // pine-motion stamps `data-pp-animating="true"` on
            // every element it kicks an animation on, then clears
            // it on a settle-bound timeout. The CSS rule on
            // `.note[data-pp-animating="true"]` does the z-index
            // lift; nothing else to wire here.
            pine_motion::play_layout(&root, snap, pine_motion::Spring::gentle());
        });
    }
}

/// Focus the first element matching `selector`, deferred until
/// after the next reactive flush so the target has actually
/// mounted by the time we look it up. Used by the new-note
/// shortcuts and the composer expand handlers.
pub fn focus_after_flush(selector: &'static str) {
    pocopine::tick::after_flush(move || {
        let Some(el) =
            pocopine::dom::document().and_then(|d| d.query_selector(selector).ok().flatten())
        else {
            return;
        };
        pocopine::focus::focus_element_no_scroll(&el);
    });
}
