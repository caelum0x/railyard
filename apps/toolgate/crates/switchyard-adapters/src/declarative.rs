//! The declarative HTTP adapter.

use async_trait::async_trait;
use serde_json::{json, Value};
use switchyard_catalog::{mapping, template};
use switchyard_core::manifest::CredentialPlacement;
use switchyard_core::{Adapter, AdapterError, AdapterOutput, CallCtx, HttpRequest, ToolManifest};

use crate::util::send;

pub struct DeclarativeAdapter {
    provider: String,
    tools: Vec<ToolManifest>,
    placement: Option<CredentialPlacement>,
}

impl DeclarativeAdapter {
    pub fn new(provider: &str, tools: Vec<ToolManifest>, placement: Option<CredentialPlacement>) -> Self {
        DeclarativeAdapter { provider: provider.into(), tools, placement }
    }

    fn render(&self, ctx: &CallCtx, m: &ToolManifest, input: &Value) -> Result<HttpRequest, AdapterError> {
        let spec = m.request.as_ref().ok_or_else(|| AdapterError::fatal(format!("{} has no request template", m.id)))?;
        let config = serde_json::to_value(&*ctx.config).unwrap_or(json!({}));
        let r = |t: &str| template::render(t, input, &config).map_err(|e| AdapterError::fatal(format!("template: {e}")));
        let base = r(&spec.url)?;
        if base.starts_with('/') || base.is_empty() {
            return Err(AdapterError::fatal(format!("{}: base URL not configured", m.id)));
        }
        let mut url = url::Url::parse(&base).map_err(|e| AdapterError::fatal(format!("{}: bad URL {base}: {e}", m.id)))?;
        {
            let mut q = url.query_pairs_mut();
            for (k, t) in &spec.query {
                let v = r(t)?;
                if !v.is_empty() {
                    q.append_pair(k, &v);
                }
            }
        }
        if url.query() == Some("") {
            url.set_query(None);
        }
        let mut req = HttpRequest::get(url.to_string());
        req.method = spec.method.to_uppercase();
        for (k, t) in &spec.headers {
            let v = r(t)?;
            if !v.is_empty() {
                req.headers.push((k.clone(), v));
            }
        }
        if let Some(body) = &spec.body {
            let b = template::render_body(body, input, &config).map_err(|e| AdapterError::fatal(format!("template: {e}")))?;
            req.body = Some(b.to_string().into_bytes());
            req.headers.push(("content-type".into(), "application/json".into()));
        }
        if let (Some(cred), Some(p)) = (&ctx.credential, &self.placement) {
            if let Some(h) = &p.header {
                req.sensitive_headers.push((h.clone(), format!("{}{}", p.prefix.clone().unwrap_or_default(), cred.expose())));
            } else if let Some(qname) = &p.query {
                let mut u = url::Url::parse(&req.url).map_err(|e| AdapterError::fatal(e.to_string()))?;
                u.query_pairs_mut().append_pair(qname, cred.expose());
                req.url = u.to_string();
            }
        }
        Ok(req)
    }
}

#[async_trait]
impl Adapter for DeclarativeAdapter {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn tools(&self) -> &[ToolManifest] {
        &self.tools
    }

    async fn call(&self, ctx: &CallCtx, tool: &str, input: Value) -> Result<AdapterOutput, AdapterError> {
        let m = self.tools.iter().find(|t| t.id == tool).ok_or_else(|| AdapterError::fatal(format!("{tool} not served here")))?;
        let req = self.render(ctx, m, &input)?;
        let resp = send(ctx, req).await?;
        if !(200..300).contains(&resp.status) {
            return Err(AdapterError::from_status(resp.status, &resp.text(), m.bill_on_4xx));
        }
        let raw = resp.json().map_err(|e| AdapterError::retryable(e).with_status(resp.status))?;
        let spec = m.request.as_ref().expect("checked in render");
        let mut data = match &spec.output {
            Some(o) => mapping::apply(o, &raw),
            None => raw,
        };
        let mut units = None;
        if let Some(ptr) = &spec.results_pointer {
            let limit = input.get("limit").and_then(Value::as_u64).map(|n| n as usize);
            if let Some(Value::Array(arr)) = data.pointer_mut(ptr) {
                if let Some(l) = limit {
                    arr.truncate(l);
                }
                units = Some(arr.len() as u64);
            }
        }
        Ok(AdapterOutput { data, status: resp.status, units, reported_cost_micros: None })
    }
}
