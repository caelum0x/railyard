//! Build one adapter per tool from the catalog.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use switchyard_catalog::Catalog;
use switchyard_core::{Adapter, ToolManifest};

use crate::declarative::DeclarativeAdapter;
use crate::native::{NativeAdapter, PublicState, KEYS};

/// tool id -> adapter serving it.
pub type AdapterMap = HashMap<String, Arc<dyn Adapter>>;

pub fn build(catalog: &Catalog) -> Result<AdapterMap, String> {
    let state = Arc::new(PublicState::default());
    let mut declarative: BTreeMap<String, Vec<ToolManifest>> = BTreeMap::new();
    let mut native: BTreeMap<String, Vec<ToolManifest>> = BTreeMap::new();
    for t in catalog.tools() {
        match (&t.request, &t.native) {
            (_, Some(k)) => {
                if !KEYS.contains(&k.as_str()) {
                    return Err(format!("tool {} names unknown native handler {k}", t.id));
                }
                native.entry(t.provider.clone()).or_default().push(t.clone());
            }
            (Some(_), None) => declarative.entry(t.provider.clone()).or_default().push(t.clone()),
            (None, None) => return Err(format!("tool {} has no adapter", t.id)),
        }
    }
    let mut map: AdapterMap = HashMap::new();
    for (provider, tools) in declarative {
        let placement = catalog.provider(&provider).and_then(|p| p.credential.clone());
        let ids: Vec<String> = tools.iter().map(|t| t.id.clone()).collect();
        let a: Arc<dyn Adapter> = Arc::new(DeclarativeAdapter::new(&provider, tools, placement));
        for id in ids {
            map.insert(id, a.clone());
        }
    }
    for (provider, tools) in native {
        let ids: Vec<String> = tools.iter().map(|t| t.id.clone()).collect();
        let a: Arc<dyn Adapter> = Arc::new(NativeAdapter::new(&provider, tools, state.clone()));
        for id in ids {
            map.insert(id, a.clone());
        }
    }
    Ok(map)
}
