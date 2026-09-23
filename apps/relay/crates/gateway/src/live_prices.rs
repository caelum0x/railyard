//! Hot-swappable retail price table shared by billing and metering.
//!
//! The price table used to be built once at startup, so an operator price edit
//! required a redeploy. [`LivePrices`] lets the catalog price editor (and the
//! periodic cross-replica refresh) swap in a rebuilt table atomically; every
//! request reads one consistent snapshot via [`LivePrices::current`].

use std::sync::{Arc, RwLock};

use relay_billing::PriceTable;

#[derive(Clone)]
pub struct LivePrices {
    inner: Arc<RwLock<Arc<PriceTable>>>,
}

impl LivePrices {
    pub fn new(table: Arc<PriceTable>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(table)),
        }
    }

    /// The price table in effect right now.
    pub fn current(&self) -> Arc<PriceTable> {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            // A poisoned lock still holds the last fully-written table: the
            // swap below is a single pointer assignment.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Atomically publish a rebuilt table.
    pub fn replace(&self, table: PriceTable) {
        let table = Arc::new(table);
        match self.inner.write() {
            Ok(mut guard) => *guard = table,
            Err(poisoned) => *poisoned.into_inner() = table,
        }
    }
}

impl From<Arc<PriceTable>> for LivePrices {
    fn from(table: Arc<PriceTable>) -> Self {
        Self::new(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_billing::{ModelId, ModelPrice};

    #[test]
    fn replace_is_visible_to_existing_handles() {
        let live = LivePrices::new(Arc::new(PriceTable::new(ModelPrice::new(1, 1))));
        let reader = live.clone();
        let mut next = PriceTable::new(ModelPrice::new(1, 1));
        next.insert(ModelId::namespace("acme/m"), ModelPrice::new(5, 5));
        live.replace(next);
        assert!(reader.current().is_known(&ModelId::namespace("acme/m")));
    }
}
