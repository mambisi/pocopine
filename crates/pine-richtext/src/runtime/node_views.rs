//! Per-`EditorRuntime` node-view registry.
//!
//! Mirror of [`crate::render::node_views`]'s `BTreeMap<String, NodeViewSpec>`
//! shape, but owned by a single [`crate::runtime::EditorRuntime`] instead
//! of a process-global `OnceLock<RwLock<...>>`. Two runtimes can each map
//! the same `node_type` (e.g. `"task_item"`) to different custom-element
//! tags without colliding. The tags themselves still have to be
//! registered process-globally with the browser via `App::register::<C>()`
//! — that's a browser constraint, not a pine-richtext one.

use std::collections::BTreeMap;

pub use crate::render::node_views::NodeViewSpec;

#[derive(Clone, Default)]
pub struct NodeViewRegistry {
    map: BTreeMap<String, NodeViewSpec>,
}

impl NodeViewRegistry {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, node_type: impl Into<String>, spec: NodeViewSpec) {
        self.map.insert(node_type.into(), spec);
    }

    pub fn lookup(&self, node_type: &str) -> Option<&NodeViewSpec> {
        self.map.get(node_type)
    }

    pub fn registered_tags(&self) -> Vec<String> {
        self.map.values().map(|spec| spec.tag.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}
