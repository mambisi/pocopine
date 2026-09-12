use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

/// Dropping a subscription disconnects it, including during finalization.
pub struct Subscription(Option<Box<dyn FnOnce()>>);

impl Subscription {
    pub fn dispose(mut self) {
        if let Some(dispose) = self.0.take() {
            dispose();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(dispose) = self.0.take() {
            dispose();
        }
    }
}

type Listener<T> = Rc<dyn Fn(&T)>;

pub(super) struct Events<T> {
    listeners: Rc<RefCell<BTreeMap<u64, Listener<T>>>>,
    next: Cell<u64>,
}

impl<T: 'static> Default for Events<T> {
    fn default() -> Self {
        Self {
            listeners: Default::default(),
            next: Cell::new(0),
        }
    }
}

impl<T: 'static> Events<T> {
    pub fn subscribe(&self, callback: impl Fn(&T) + 'static) -> Subscription {
        let id = self.next.get();
        self.next.set(id + 1);
        self.listeners.borrow_mut().insert(id, Rc::new(callback));
        let weak = Rc::downgrade(&self.listeners);
        Subscription(Some(Box::new(move || {
            if let Some(listeners) = weak.upgrade() {
                listeners.borrow_mut().remove(&id);
            }
        })))
    }

    pub fn emit(&self, event: &T) {
        // A listener may read the model or dispose another listener. Never hold
        // a registry/model borrow across an application callback.
        let ids = self.listeners.borrow().keys().copied().collect::<Vec<_>>();
        for id in ids {
            let callback = self.listeners.borrow().get(&id).cloned();
            if let Some(callback) = callback {
                callback(event);
            }
        }
    }

    pub fn clear(&self) {
        self.listeners.borrow_mut().clear();
    }
}
