//! Human-browsable catalog (backlog item 13): `GET /catalog` renders every
//! capability and its tools with live pricing and a copy-paste `curl`
//! snippet, reusing the same signup flow (`/v1/auth/signup`) as item 6's
//! onboarding. `GET /v1/catalog` answers the same data as JSON for tooling.

use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde_json::json;
use switchyard_core::{Price, PriceBasis, ToolManifest};

use crate::http_util::html_escape as esc;
use crate::state::AppState;

fn basis_label(b: &PriceBasis) -> &'static str {
    match b {
        PriceBasis::PerCall => "call",
        PriceBasis::PerSuccess => "success",
        PriceBasis::PerResult => "result",
        PriceBasis::PerUnit => "unit",
    }
}

fn price_label(p: &Price) -> String {
    if p.micros == 0 {
        return "free".to_string();
    }
    let dollars = p.micros as f64 / 1_000_000.0;
    let unit = p.unit.as_deref().unwrap_or_else(|| basis_label(&p.basis));
    if dollars >= 0.01 {
        format!("${dollars:.4} / {unit}")
    } else {
        format!("{} \u{b5}$ / {unit}", p.micros)
    }
}

/// A copy-paste `curl` snippet. The example input comes from the catalog's
/// own `example_input` (the manifest's documented example when one exists,
/// otherwise a schema-derived placeholder for every required field) so a
/// real developer pasting this straight from `/catalog` gets a real response
/// instead of an opaque `422 invalid_input` on a tool that requires fields --
/// a bare `{"input": {}}` failed this for most of the catalog (13/27 tools
/// ship a manifest example; most of the rest still have required fields).
fn curl_snippet(base: &str, tool: &ToolManifest, catalog: &switchyard_catalog::Catalog) -> String {
    let input = catalog.example_input(&tool.id);
    let input_json = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
    format!(
        "curl {base}/v1/tools/{}/call \\\n  -H \"Authorization: Bearer sk-t0-...\" \\\n  -H \"Content-Type: application/json\" \\\n  -d '{{\"input\": {input_json}}}'",
        tool.id
    )
}

fn catalog_json(st: &AppState) -> serde_json::Value {
    let caps: Vec<serde_json::Value> = st
        .catalog
        .capabilities()
        .map(|c| {
            let tools: Vec<serde_json::Value> = st
                .catalog
                .tools_for_capability(&c.id)
                .iter()
                .map(|t| json!({ "id": t.id, "provider": t.provider, "title": t.title, "side_effect": t.side_effect.as_str(), "price_micros": t.price.micros, "price_basis": basis_label(&t.price.basis), "price_label": price_label(&t.price) }))
                .collect();
            json!({ "id": c.id, "title": c.title, "description": c.description, "tags": c.tags, "tools": tools })
        })
        .collect();
    json!({ "base_url": st.settings.public_url, "capabilities": caps, "signup_url": format!("{}/v1/auth/signup", st.settings.public_url) })
}

pub async fn as_json(State(st): State<AppState>) -> Response {
    Json(catalog_json(&st)).into_response()
}

pub async fn html(State(st): State<AppState>) -> Response {
    let base = &st.settings.public_url;
    let mut sections = String::new();
    for c in st.catalog.capabilities() {
        let tools = st.catalog.tools_for_capability(&c.id);
        let mut rows = String::new();
        for t in &tools {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><pre class=\"snippet\">{}</pre></td></tr>",
                esc(&t.id),
                esc(&t.provider),
                esc(t.side_effect.as_str()),
                esc(&price_label(&t.price)),
                esc(&curl_snippet(base, t, &st.catalog)),
            ));
        }
        if rows.is_empty() {
            continue;
        }
        sections.push_str(&format!(
            "<section><h2>{}</h2><p class=\"muted\">{}</p><table><thead><tr><th>Tool</th><th>Provider</th><th>Effect</th><th>Price</th><th>Call it</th></tr></thead><tbody>{}</tbody></table></section>",
            esc(&c.title),
            esc(&c.description),
            rows,
        ));
    }
    let doc = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Switchyard catalog -- 24 capabilities, tools and live pricing</title>
<style>
body{{font:15px/1.5 system-ui,-apple-system,sans-serif;margin:0;background:#0b0e14;color:#e6e9ef}}
header{{padding:36px 28px 20px;border-bottom:1px solid #1e2430}}
header h1{{margin:0 0 8px;font-size:26px}}
header p{{color:#9aa4b2;max-width:720px;margin:0 0 14px}}
.cta{{display:inline-block;background:#5b8cff;color:#05070c;padding:10px 16px;border-radius:8px;text-decoration:none;font-weight:600}}
main{{padding:8px 28px 60px;max-width:1100px}}
section{{margin:34px 0;background:#121725;border:1px solid #1e2430;border-radius:12px;padding:18px 20px;overflow:auto}}
section h2{{margin:0 0 4px;font-size:17px}}
.muted{{color:#9aa4b2;font-size:13px;margin:0 0 14px}}
table{{border-collapse:collapse;width:100%;font-size:13px}}
th,td{{text-align:left;padding:8px 10px;border-bottom:1px solid #1e2430;vertical-align:top}}
th{{color:#9aa4b2;font-weight:600}}
pre.snippet{{margin:0;white-space:pre-wrap;font-size:12px;background:#0b0e14;border:1px solid #1e2430;border-radius:6px;padding:8px;max-width:420px}}
code{{color:#8fd6ff}}
</style></head>
<body>
<header>
<h1>Switchyard catalog</h1>
<p>One <code>sk-t0-</code> key reaches every tool below: our own products first, then free and public routes, then your own keys. Every call names its cost in micro-USD up front; failures are never billed.</p>
<a class=\"cta\" href=\"{base}/v1/auth/signup\">Get a key -- POST /v1/auth/signup</a>
&nbsp; <a href=\"{base}/llms.txt\" style=\"color:#9aa4b2\">llms.txt</a>
&nbsp; <a href=\"{base}/openapi.json\" style=\"color:#9aa4b2\">openapi.json</a>
</header>
<main>{sections}</main>
</body></html>"
    );
    Html(doc).into_response()
}
