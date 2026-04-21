//! Recent-route history ring for the router panel.
//!
//! The devtools `on_route_change` handler pushes `(path, params)`
//! tuples here on every router mount. The router panel snapshots
//! the last [`CAP`] entries and renders them as a scrollable list
//! with the newest at the top.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

pub(crate) const CAP: usize = 40;

#[derive(Clone, Debug)]
pub(crate) struct RouteEntry {
    pub path: String,
    pub params: HashMap<String, String>,
    /// `performance.now()` timestamp in ms.
    pub t_ms: f64,
}

thread_local! {
    static LOG: RefCell<VecDeque<RouteEntry>> =
        RefCell::new(VecDeque::with_capacity(CAP));
}

pub(crate) fn push(path: &str, params: &HashMap<String, String>) {
    let entry = RouteEntry {
        path: path.to_string(),
        params: params.clone(),
        t_ms: super::ring::now_ms_for_scope(),
    };
    LOG.with(|l| {
        let mut v = l.borrow_mut();
        if v.len() == CAP {
            v.pop_front();
        }
        v.push_back(entry);
    });
}

pub(crate) fn snapshot() -> Vec<RouteEntry> {
    LOG.with(|l| l.borrow().iter().cloned().collect())
}

pub(crate) fn len() -> usize {
    LOG.with(|l| l.borrow().len())
}
