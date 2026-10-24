//! Catalog loading and integrity checks.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use switchyard_core::{CapabilityManifest, ProviderManifest, ToolManifest};

use crate::schema::Schema;
use crate::search::{SearchFilters, SearchHit, SearchIndex};

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("io error reading {path}: {message}")]
    Io { path: String, message: String },
    #[error("parse error in {path}: {message}")]
    Parse { path: String, message: String },
    #[error("invalid catalog: {0}")]
    Invalid(String),
}

/// A SKILL.md bundle shipped with the catalog (public) or pushed by a team.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tool_ids: Vec<String>,
    #[serde(default)]
    pub credential_refs: Vec<String>,
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default, skip_deserializing)]
    pub skill_md: String,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            OneOrMany::Many(v) => v,
            OneOrMany::One(t) => vec![t],
        }
    }
}

pub struct Catalog {
    capabilities: BTreeMap<String, CapabilityManifest>,
    providers: BTreeMap<String, ProviderManifest>,
    tools: BTreeMap<String, ToolManifest>,
    input_schemas: HashMap<String, Schema>,
    bundles: BTreeMap<String, BundleManifest>,
    index: SearchIndex,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog").field("tools", &self.tools.len()).field("capabilities", &self.capabilities.len()).finish()
    }
}

fn read_yaml_dir<T: for<'de> Deserialize<'de>>(dir: &Path) -> Result<Vec<T>, CatalogError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let is_yaml = matches!(path.extension().and_then(|e| e.to_str()), Some("yaml") | Some("yml"));
        if !is_yaml {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CatalogError::Io { path: path.display().to_string(), message: e.to_string() })?;
        let parsed: OneOrMany<T> = serde_yaml::from_str(&text)
            .map_err(|e| CatalogError::Parse { path: path.display().to_string(), message: e.to_string() })?;
        out.extend(parsed.into_vec());
    }
    Ok(out)
}

impl Catalog {
    /// Load `dir/{capabilities,providers,tools}/*.yaml` and `dir/bundles/*/`.
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let dir = dir.as_ref();
        let capabilities = read_yaml_dir::<CapabilityManifest>(&dir.join("capabilities"))?;
        let providers = read_yaml_dir::<ProviderManifest>(&dir.join("providers"))?;
        let tools = read_yaml_dir::<ToolManifest>(&dir.join("tools"))?;
        let mut bundles = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir.join("bundles")) {
            let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
            paths.sort();
            for p in paths {
                let meta_path = p.join("bundle.yaml");
                let text = std::fs::read_to_string(&meta_path)
                    .map_err(|e| CatalogError::Io { path: meta_path.display().to_string(), message: e.to_string() })?;
                let mut b: BundleManifest = serde_yaml::from_str(&text)
                    .map_err(|e| CatalogError::Parse { path: meta_path.display().to_string(), message: e.to_string() })?;
                let skill_path = p.join("SKILL.md");
                b.skill_md = std::fs::read_to_string(&skill_path)
                    .map_err(|e| CatalogError::Io { path: skill_path.display().to_string(), message: e.to_string() })?;
                bundles.push(b);
            }
        }
        Self::from_parts(capabilities, providers, tools, bundles)
    }

    /// Build and verify a catalog from parsed manifests.
    pub fn from_parts(
        capabilities: Vec<CapabilityManifest>,
        providers: Vec<ProviderManifest>,
        tools: Vec<ToolManifest>,
        bundles: Vec<BundleManifest>,
    ) -> Result<Self, CatalogError> {
        let mut caps = BTreeMap::new();
        for c in capabilities {
            if caps.insert(c.id.clone(), c.clone()).is_some() {
                return Err(CatalogError::Invalid(format!("duplicate capability {}", c.id)));
            }
        }
        let mut provs = BTreeMap::new();
        for p in providers {
            if provs.insert(p.id.clone(), p.clone()).is_some() {
                return Err(CatalogError::Invalid(format!("duplicate provider {}", p.id)));
            }
        }
        let mut tool_map = BTreeMap::new();
        let mut input_schemas = HashMap::new();
        for c in caps.values() {
            let s = Schema::compile(&c.input_schema)
                .map_err(|e| CatalogError::Invalid(format!("capability {} input schema: {e}", c.id)))?;
            Schema::compile(&c.output_schema)
                .map_err(|e| CatalogError::Invalid(format!("capability {} output schema: {e}", c.id)))?;
            input_schemas.insert(c.id.clone(), s);
        }
        for t in tools {
            if caps.contains_key(&t.id) {
                return Err(CatalogError::Invalid(format!("tool id {} collides with a capability id", t.id)));
            }
            if !caps.contains_key(&t.capability) {
                return Err(CatalogError::Invalid(format!("tool {} names unknown capability {}", t.id, t.capability)));
            }
            if !provs.contains_key(&t.provider) {
                return Err(CatalogError::Invalid(format!("tool {} names unknown provider {}", t.id, t.provider)));
            }
            if t.request.is_none() && t.native.is_none() {
                return Err(CatalogError::Invalid(format!("tool {} has neither `request` nor `native`", t.id)));
            }
            if t.price.micros < 0 {
                return Err(CatalogError::Invalid(format!("tool {} has a negative price", t.id)));
            }
            let schema_value = t.input_schema.clone().unwrap_or_else(|| caps[&t.capability].input_schema.clone());
            let schema = Schema::compile(&schema_value)
                .map_err(|e| CatalogError::Invalid(format!("tool {} input schema: {e}", t.id)))?;
            if let Some(ex) = &t.example {
                schema.validate(&ex.input).map_err(|errs| {
                    CatalogError::Invalid(format!("tool {} example input fails its own schema: {errs:?}", t.id))
                })?;
            }
            input_schemas.insert(t.id.clone(), schema);
            if tool_map.insert(t.id.clone(), t.clone()).is_some() {
                return Err(CatalogError::Invalid(format!("duplicate tool {}", t.id)));
            }
        }
        let mut bundle_map = BTreeMap::new();
        for b in bundles {
            for tid in &b.tool_ids {
                if !tool_map.contains_key(tid) && !caps.contains_key(tid) {
                    return Err(CatalogError::Invalid(format!("bundle {} refers to unknown tool {tid}", b.id)));
                }
            }
            bundle_map.insert(b.id.clone(), b);
        }
        let index = SearchIndex::build(&caps, &tool_map);
        Ok(Catalog { capabilities: caps, providers: provs, tools: tool_map, input_schemas, bundles: bundle_map, index })
    }

    pub fn tool(&self, id: &str) -> Option<&ToolManifest> {
        self.tools.get(id)
    }
    pub fn capability(&self, id: &str) -> Option<&CapabilityManifest> {
        self.capabilities.get(id)
    }
    pub fn provider(&self, id: &str) -> Option<&ProviderManifest> {
        self.providers.get(id)
    }
    pub fn tools(&self) -> impl Iterator<Item = &ToolManifest> {
        self.tools.values()
    }
    pub fn capabilities(&self) -> impl Iterator<Item = &CapabilityManifest> {
        self.capabilities.values()
    }
    pub fn providers(&self) -> impl Iterator<Item = &ProviderManifest> {
        self.providers.values()
    }
    pub fn bundles(&self) -> impl Iterator<Item = &BundleManifest> {
        self.bundles.values()
    }
    pub fn bundle(&self, id: &str) -> Option<&BundleManifest> {
        self.bundles.get(id)
    }
    pub fn tools_for_capability(&self, capability: &str) -> Vec<&ToolManifest> {
        self.tools.values().filter(|t| t.capability == capability && t.status == "active").collect()
    }

    /// The effective input schema for a tool or capability id.
    pub fn input_schema(&self, id: &str) -> Option<Value> {
        if let Some(t) = self.tools.get(id) {
            return Some(t.input_schema.clone().unwrap_or_else(|| self.capabilities[&t.capability].input_schema.clone()));
        }
        self.capabilities.get(id).map(|c| c.input_schema.clone())
    }

    /// The effective output schema for a tool or capability id.
    pub fn output_schema(&self, id: &str) -> Option<Value> {
        if let Some(t) = self.tools.get(id) {
            return Some(t.output_schema.clone().unwrap_or_else(|| self.capabilities[&t.capability].output_schema.clone()));
        }
        self.capabilities.get(id).map(|c| c.output_schema.clone())
    }

    /// A real, runnable example input for a tool or capability: the
    /// manifest's own documented `example.input` when the catalog author
    /// provided one, otherwise a placeholder derived from the effective
    /// input schema's `required` properties -- never a bare `{}` for a tool
    /// that would reject it. Used anywhere an example is shown as a
    /// copy-paste call (the `/catalog` page's curl snippets, `sy describe`'s
    /// dry-run preview), so those examples pass the exact same JSON Schema
    /// validation a real call enforces instead of failing with
    /// `invalid_input` on first use.
    pub fn example_input(&self, id: &str) -> Value {
        if let Some(t) = self.tools.get(id) {
            if let Some(ex) = &t.example {
                return ex.input.clone();
            }
        }
        self.input_schema(id).map(|schema| placeholder_from_schema(&schema)).unwrap_or_else(|| Value::Object(Default::default()))
    }

    /// Validate an input for a tool or capability id.
    pub fn validate_input(&self, id: &str, input: &Value) -> Result<(), Vec<(String, String)>> {
        match self.input_schemas.get(id) {
            Some(s) => s.validate(input),
            None => Err(vec![(String::new(), format!("unknown tool or capability {id}"))]),
        }
    }

    /// Union of all provider egress hosts (the static allowlist).
    pub fn egress_hosts(&self) -> BTreeSet<String> {
        self.providers.values().flat_map(|p| p.egress_hosts.iter().cloned()).collect()
    }

    pub fn search(&self, query: &str, filters: &SearchFilters) -> Vec<SearchHit> {
        self.index.search(self, query, filters)
    }
}

/// Build a minimal JSON object covering every property a JSON Schema
/// `object` marks required, so the result satisfies schema validation. A
/// `default` on the property wins; otherwise a representative literal for
/// its declared `type` (first type when the schema lists several, e.g.
/// `["string", "null"]`), pattern-checked when the property has a `pattern`
/// so e.g. a `domain` field gets `example.com` instead of a plain word that
/// fails its own regex. "Required" here is either the schema's own
/// top-level `required` list, or -- when a tool needs one of several fields
/// (`company.sec.filings`: ticker OR cik) -- the first `anyOf` alternative's
/// `required` list. Properties without a `type`/`default` and non-object
/// schemas fall back to an empty object -- strictly better than a bare `{}`
/// for anything with required fields, never worse.
fn placeholder_from_schema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else { return Value::Object(Default::default()) };
    let Some(props) = obj.get("properties").and_then(Value::as_object) else {
        return Value::Object(Default::default());
    };
    let mut required: Vec<&str> = obj.get("required").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if required.is_empty() {
        if let Some(first_alt) = obj.get("anyOf").and_then(Value::as_array).and_then(|a| a.first()) {
            required = first_alt.get("required").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        }
    }
    let mut out = serde_json::Map::new();
    for name in required {
        let Some(prop) = props.get(name) else { continue };
        let value = prop
            .get("default")
            .cloned()
            .or_else(|| prop.get("enum").and_then(Value::as_array).and_then(|a| a.first().cloned()))
            .unwrap_or_else(|| placeholder_for_type(prop));
        out.insert(name.to_string(), value);
    }
    Value::Object(out)
}

/// A handful of candidate strings that look like real-world values (a
/// domain, an email, a short word), tried in order against the property's
/// own `pattern` when it has one. Falls back to the first candidate
/// ("example") when there is no pattern or none of the candidates match --
/// still correct for the common unconstrained-string case, and no worse
/// than before for a pattern we didn't anticipate.
fn placeholder_for_type(prop: &Value) -> Value {
    let kind = match prop.get("type") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Array(a)) => a.iter().find_map(|v| v.as_str()).filter(|s| *s != "null"),
        _ => None,
    };
    match kind {
        Some("string") => Value::String(string_placeholder(prop).into()),
        Some("integer") => Value::Number(1.into()),
        Some("number") => serde_json::Number::from_f64(1.0).map(Value::Number).unwrap_or(Value::Null),
        Some("boolean") => Value::Bool(true),
        Some("array") => Value::Array(vec![]),
        Some("object") => Value::Object(Default::default()),
        _ => Value::String(string_placeholder(prop).into()),
    }
}

fn string_placeholder(prop: &Value) -> &'static str {
    const CANDIDATES: &[&str] = &["example", "example.com", "example@example.com", "https://example.com", "1"];
    match prop.get("pattern").and_then(Value::as_str).and_then(|p| regex::Regex::new(p).ok()) {
        Some(re) => CANDIDATES.iter().copied().find(|c| re.is_match(c)).unwrap_or("example"),
        None => "example",
    }
}
