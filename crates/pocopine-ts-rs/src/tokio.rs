use tokio::sync::{Mutex, OnceCell, RwLock};

use super::{TS, TypeVisitor, impl_wrapper};

impl_wrapper!(impl<T: TS> TS for Mutex<T>);
impl_wrapper!(impl<T: TS> TS for OnceCell<T>);
impl_wrapper!(impl<T: TS> TS for RwLock<T>);
