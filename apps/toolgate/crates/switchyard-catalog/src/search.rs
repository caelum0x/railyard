//! Search by task: BM25 over capability and tool text, boosted by free-first
//! status and observed success rate. Tiny and in-process (a few hundred tools
//! fit in well under a megabyte).

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use switchyard_core::{AuthMode, CapabilityManifest, SideEffect, ToolManifest};

use crate::loader::Catalog;

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub side_effect: Option<SideEffect>,
    pub max_price_micros: Option<i64>,
    pub limit: usize,
    /// Observed success rate per tool id (0..1), from provider stats.
    pub success_rate: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateView {
    pub tool_id: String,
    pub provider: String,
    pub provider_kind: String,
    pub title: String,
    pub side_effect: String,
    pub price: switchyard_core::Price,
    pub free: bool,
    pub requires_byo_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub capability: String,
    pub title: String,
    pub description: String,
    pub score: f64,
    pub candidates: Vec<CandidateView>,
}

struct Doc {
    capability: String,
    terms: HashMap<String, u32>,
    len: u32,
}

pub struct SearchIndex {
    docs: Vec<Doc>,
    df: HashMap<String, u32>,
    avg_len: f64,
}

/// Lowercase alphanumeric tokens with light plural stemming and a few
/// task-language synonyms so "look up the weather" finds `weather.forecast`.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let w = raw.to_lowercase();
        if STOP.contains(&w.as_str()) {
            continue;
        }
        let stem = stem(&w);
        for syn in synonyms(&stem) {
            out.push(syn.to_string());
        }
        out.push(stem);
    }
    out
}

const STOP: &[&str] = &["a", "an", "the", "of", "for", "to", "in", "on", "and", "or", "by", "with", "me", "my", "from", "find", "get", "i"];

fn stem(w: &str) -> String {
    if w.len() > 4 && w.ends_with("ies") {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

fn synonyms(w: &str) -> &'static [&'static str] {
    match w {
        "mail" | "e" => &["email"],
        "whois" | "registrar" => &["domain"],
        "lat" | "lon" | "coordinate" | "address" | "place" => &["geocode"],
        "paper" | "scholar" | "academic" => &["research"],
        "repo" | "repository" => &["github"],
        "llm" | "gpt" | "model" | "completion" => &["chat"],
        "voice" | "speech" | "audio" => &["tts"],
        "sql" | "postgres" => &["database"],
        "lead" | "prospect" => &["people", "company"],
        "filing" | "10k" | "edgar" => &["sec"],
        "news" | "hacker" => &["hn"],
        "forecast" | "rain" | "temperature" => &["weather"],
        _ => &[],
    }
}

impl SearchIndex {
    pub fn build(caps: &BTreeMap<String, CapabilityManifest>, tools: &BTreeMap<String, ToolManifest>) -> Self {
        let mut docs = Vec::new();
        let mut df: HashMap<String, u32> = HashMap::new();
        for cap in caps.values() {
            let mut text = format!("{} {} {} {}", cap.id.replace('.', " "), cap.title, cap.description, cap.tags.join(" "));
            for t in tools.values().filter(|t| t.capability == cap.id) {
                text.push(' ');
                text.push_str(&format!("{} {} {} {} {}", t.id.replace('.', " "), t.title, t.description, t.tags.join(" "), t.provider));
            }
            let toks = tokenize(&text);
            let mut terms: HashMap<String, u32> = HashMap::new();
            for t in &toks {
                *terms.entry(t.clone()).or_default() += 1;
            }
            for t in terms.keys() {
                *df.entry(t.clone()).or_default() += 1;
            }
            docs.push(Doc { capability: cap.id.clone(), len: toks.len() as u32, terms });
        }
        let avg_len = if docs.is_empty() { 1.0 } else { docs.iter().map(|d| d.len as f64).sum::<f64>() / docs.len() as f64 };
        SearchIndex { docs, df, avg_len }
    }

    pub fn search(&self, catalog: &Catalog, query: &str, filters: &SearchFilters) -> Vec<SearchHit> {
        let q = tokenize(query);
        let n = self.docs.len() as f64;
        let (k1, b) = (1.2, 0.75);
        let mut hits = Vec::new();
        for doc in &self.docs {
            let mut score = 0.0;
            for term in &q {
                let tf = *doc.terms.get(term).unwrap_or(&0) as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = *self.df.get(term).unwrap_or(&0) as f64;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                score += idf * (tf * (k1 + 1.0)) / (tf + k1 * (1.0 - b + b * doc.len as f64 / self.avg_len));
            }
            if score <= 0.0 && !q.is_empty() {
                continue;
            }
            let cap = match catalog.capability(&doc.capability) {
                Some(c) => c,
                None => continue,
            };
            let mut candidates: Vec<CandidateView> = catalog
                .tools_for_capability(&cap.id)
                .into_iter()
                .filter(|t| filters.side_effect.map(|s| s == t.side_effect).unwrap_or(true))
                .filter(|t| filters.max_price_micros.map(|m| t.price.micros <= m).unwrap_or(true))
                .map(|t| {
                    let kind = catalog.provider(&t.provider).map(|p| p.kind);
                    CandidateView {
                        tool_id: t.id.clone(),
                        provider: t.provider.clone(),
                        provider_kind: kind.map(|k| k.as_str().to_string()).unwrap_or_default(),
                        title: t.title.clone(),
                        side_effect: t.side_effect.as_str().to_string(),
                        price: t.price.clone(),
                        free: t.price.is_free(),
                        requires_byo_key: t.auth == AuthMode::ByoRequired,
                        success_rate: filters.success_rate.get(&t.id).copied(),
                    }
                })
                .collect();
            if candidates.is_empty() {
                continue;
            }
            candidates.sort_by_key(|c| (!c.free, c.requires_byo_key, c.price.micros));
            // Boosts: a free candidate (+15%), observed reliability (up to +10%).
            if candidates.iter().any(|c| c.free && !c.requires_byo_key) {
                score *= 1.15;
            }
            let best_rate = candidates.iter().filter_map(|c| c.success_rate).fold(None, |a: Option<f64>, r| Some(a.map_or(r, |x| x.max(r))));
            if let Some(r) = best_rate {
                score *= 0.9 + 0.2 * r.clamp(0.0, 1.0);
            }
            hits.push(SearchHit { capability: cap.id.clone(), title: cap.title.clone(), description: cap.description.clone(), score, candidates });
        }
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(if filters.limit == 0 { 10 } else { filters.limit });
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_stems_and_expands() {
        let t = tokenize("Verify the emails of leads");
        assert!(t.contains(&"verify".to_string()));
        assert!(t.contains(&"email".to_string()));
        assert!(t.contains(&"people".to_string()));
        assert!(!t.contains(&"the".to_string()));
    }
}
