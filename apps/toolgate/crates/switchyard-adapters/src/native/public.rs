//! Native adapters for public APIs whose shape needs code.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use switchyard_core::{AdapterError, AdapterOutput, CallCtx, HttpRequest};

use crate::util::{host_of, send, send_json, str_field};

/// Cached lookup tables (RDAP bootstrap, SEC tickers), refreshed daily.
#[derive(Default)]
pub struct PublicState {
    rdap: Mutex<Option<(Instant, Value)>>,
    tickers: Mutex<Option<(Instant, Value)>>,
}

const DAY: Duration = Duration::from_secs(86_400);

fn fresh(slot: &Mutex<Option<(Instant, Value)>>) -> Option<Value> {
    slot.lock().expect("cache poisoned").as_ref().filter(|(t, _)| t.elapsed() < DAY).map(|(_, v)| v.clone())
}

fn limit(input: &Value, default: u64) -> usize {
    input.get("limit").and_then(Value::as_u64).unwrap_or(default) as usize
}

// ---------------------------------------------------------------- arXiv

pub fn parse_arxiv(xml: &str, max: usize) -> Result<Vec<Value>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("arXiv returned invalid XML: {e}"))?;
    let text = |n: roxmltree::Node, name: &str| n.children().find(|c| c.tag_name().name() == name).and_then(|c| c.text()).map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
    Ok(doc
        .descendants()
        .filter(|n| n.tag_name().name() == "entry")
        .take(max)
        .map(|e| {
            let authors: Vec<String> = e.children().filter(|c| c.tag_name().name() == "author").filter_map(|a| text(a, "name")).collect();
            let year = text(e, "published").and_then(|p| p.get(0..4).and_then(|y| y.parse::<i64>().ok()));
            json!({ "title": text(e, "title"), "authors": authors, "year": year, "url": text(e, "id"), "doi": text(e, "doi") })
        })
        .collect())
}

pub async fn arxiv(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let q = str_field(input, "q")?;
    let n = limit(input, 10);
    let mut u = url::Url::parse("https://export.arxiv.org/api/query").expect("static url");
    u.query_pairs_mut().append_pair("search_query", &format!("all:{q}")).append_pair("max_results", &n.to_string());
    let resp = send(ctx, HttpRequest::get(u.to_string()).header("accept", "application/atom+xml")).await?;
    if !(200..300).contains(&resp.status) {
        return Err(AdapterError::from_status(resp.status, &resp.text(), false));
    }
    let results = parse_arxiv(&resp.text(), n).map_err(AdapterError::retryable)?;
    let count = results.len() as u64;
    Ok(AdapterOutput::ok(json!({ "results": results })).with_units(count))
}

// ------------------------------------------------------ package registries

fn repo_url(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("url").and_then(Value::as_str).map(|s| s.trim_start_matches("git+").to_string()),
        _ => None,
    }
}

pub async fn package(ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let eco = str_field(input, "ecosystem")?;
    let name = str_field(input, "name")?;
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let data = match eco {
        "npm" => {
            let (_, v) = send_json(ctx, HttpRequest::get(format!("https://registry.npmjs.org/{}", enc(name))).header("accept", "application/json"), false).await?;
            let latest = v.pointer("/dist-tags/latest").and_then(Value::as_str).map(str::to_string);
            let ver = latest.as_ref().and_then(|l| v.pointer(&format!("/versions/{}", l.replace('~', "~0").replace('/', "~1"))).cloned()).unwrap_or(Value::Null);
            json!({
                "ecosystem": "npm", "name": name, "version": latest,
                "description": v.get("description").or_else(|| ver.get("description")),
                "license": v.get("license").or_else(|| ver.get("license")).and_then(|l| l.as_str().map(str::to_string).or_else(|| l.get("type").and_then(Value::as_str).map(str::to_string))),
                "homepage": v.get("homepage"),
                "repository": v.get("repository").and_then(repo_url),
            })
        }
        "pypi" => {
            let (_, v) = send_json(ctx, HttpRequest::get(format!("https://pypi.org/pypi/{}/json", enc(name))), false).await?;
            let info = v.get("info").cloned().unwrap_or(Value::Null);
            let urls = info.get("project_urls").cloned().unwrap_or(Value::Null);
            json!({
                "ecosystem": "pypi", "name": name,
                "version": info.get("version"),
                "description": info.get("summary"),
                "license": info.get("license").filter(|l| l.as_str().map(|s| !s.is_empty() && s.len() < 200).unwrap_or(false)),
                "homepage": info.get("home_page").filter(|h| h.as_str().map(|s| !s.is_empty()).unwrap_or(false)).or_else(|| urls.get("Homepage")),
                "repository": urls.get("Source").or_else(|| urls.get("Repository")).or_else(|| urls.get("Source Code")),
            })
        }
        "crates" => {
            let (_, v) = send_json(ctx, HttpRequest::get(format!("https://crates.io/api/v1/crates/{}", enc(name))), false).await?;
            let c = v.get("crate").cloned().unwrap_or(Value::Null);
            json!({
                "ecosystem": "crates", "name": name,
                "version": c.get("max_stable_version").filter(|x| !x.is_null()).or_else(|| c.get("newest_version")),
                "description": c.get("description"),
                "license": v.pointer("/versions/0/license"),
                "homepage": c.get("homepage"),
                "repository": c.get("repository"),
            })
        }
        other => return Err(AdapterError::rejected(format!("unknown ecosystem {other}"))),
    };
    Ok(AdapterOutput::ok(data))
}

// ------------------------------------------------------------- SEC EDGAR

pub fn filings_from_submissions(cik: &str, sub: &Value, form: Option<&str>, max: usize) -> Value {
    let recent = sub.pointer("/filings/recent").cloned().unwrap_or(Value::Null);
    let col = |k: &str| recent.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let (forms, dates, accs, docs) = (col("form"), col("filingDate"), col("accessionNumber"), col("primaryDocument"));
    let cik_num = cik.trim_start_matches('0');
    let mut out = Vec::new();
    for (i, f) in forms.iter().enumerate() {
        let f = f.as_str().unwrap_or("");
        if form.map(|want| !f.eq_ignore_ascii_case(want)).unwrap_or(false) {
            continue;
        }
        let acc = accs.get(i).and_then(Value::as_str).unwrap_or("");
        let doc = docs.get(i).and_then(Value::as_str).unwrap_or("");
        out.push(json!({
            "form": f,
            "filed": dates.get(i),
            "accession": acc,
            "url": format!("https://www.sec.gov/Archives/edgar/data/{cik_num}/{}/{doc}", acc.replace('-', "")),
        }));
        if out.len() >= max {
            break;
        }
    }
    json!({ "cik": cik, "name": sub.get("name"), "filings": out })
}

pub async fn sec_filings(state: &PublicState, ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let cik = match input.get("cik").and_then(Value::as_str) {
        Some(c) => format!("{:0>10}", c),
        None => {
            let ticker = str_field(input, "ticker")?.to_ascii_uppercase();
            let table = match fresh(&state.tickers) {
                Some(t) => t,
                None => {
                    let (_, t) = send_json(ctx, HttpRequest::get("https://www.sec.gov/files/company_tickers.json"), false).await?;
                    *state.tickers.lock().expect("cache poisoned") = Some((Instant::now(), t.clone()));
                    t
                }
            };
            let hit = table
                .as_object()
                .and_then(|o| o.values().find(|e| e.get("ticker").and_then(Value::as_str).map(|t| t.eq_ignore_ascii_case(&ticker)).unwrap_or(false)))
                .and_then(|e| e.get("cik_str").and_then(Value::as_u64))
                .ok_or_else(|| AdapterError::rejected(format!("unknown ticker {ticker}")).with_status(404))?;
            format!("{hit:010}")
        }
    };
    let (_, sub) = send_json(ctx, HttpRequest::get(format!("https://data.sec.gov/submissions/CIK{cik}.json")), false).await?;
    let data = filings_from_submissions(&cik, &sub, input.get("form").and_then(Value::as_str), limit(input, 20));
    let n = data["filings"].as_array().map(|a| a.len() as u64).unwrap_or(0);
    Ok(AdapterOutput::ok(data).with_units(n))
}

// ------------------------------------------------------------------ RDAP

pub fn rdap_base_for(bootstrap: &Value, domain: &str) -> Option<String> {
    let tld = domain.rsplit('.').next()?.to_ascii_lowercase();
    bootstrap.get("services")?.as_array()?.iter().find_map(|svc| {
        let tlds = svc.get(0)?.as_array()?;
        if tlds.iter().any(|t| t.as_str().map(|s| s.eq_ignore_ascii_case(&tld)).unwrap_or(false)) {
            svc.get(1)?.as_array()?.iter().filter_map(Value::as_str).find(|u| u.starts_with("https://")).map(str::to_string)
        } else {
            None
        }
    })
}

pub fn summarise_rdap(domain: &str, v: &Value) -> Value {
    let events = v.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let event = |action: &str| events.iter().find(|e| e.get("eventAction").and_then(Value::as_str) == Some(action)).and_then(|e| e.get("eventDate")).cloned();
    let registrar = v.get("entities").and_then(Value::as_array).and_then(|ents| {
        ents.iter().find(|e| e.get("roles").and_then(Value::as_array).map(|r| r.iter().any(|x| x == "registrar")).unwrap_or(false)).and_then(|e| {
            e.pointer("/vcardArray/1").and_then(Value::as_array).and_then(|cards| {
                cards.iter().find(|c| c.get(0).and_then(Value::as_str) == Some("fn")).and_then(|c| c.get(3)).and_then(Value::as_str).map(str::to_string)
            })
        })
    });
    let ns: Vec<Value> = v.get("nameservers").and_then(Value::as_array).map(|a| a.iter().filter_map(|n| n.get("ldhName").cloned()).collect()).unwrap_or_default();
    json!({
        "domain": domain,
        "registrar": registrar,
        "created": event("registration"),
        "expires": event("expiration"),
        "status": v.get("status").cloned().unwrap_or(json!([])),
        "nameservers": ns,
    })
}

pub async fn rdap(state: &PublicState, ctx: &CallCtx, input: &Value) -> Result<AdapterOutput, AdapterError> {
    let domain = str_field(input, "domain")?.to_ascii_lowercase();
    let bootstrap = match fresh(&state.rdap) {
        Some(b) => b,
        None => {
            let (_, b) = send_json(ctx, HttpRequest::get("https://data.iana.org/rdap/dns.json"), false).await?;
            *state.rdap.lock().expect("cache poisoned") = Some((Instant::now(), b.clone()));
            b
        }
    };
    let base = rdap_base_for(&bootstrap, &domain).ok_or_else(|| AdapterError::rejected(format!("no RDAP server for {domain}")).with_status(404))?;
    let url = format!("{}/domain/{domain}", base.trim_end_matches('/'));
    let mut req = HttpRequest::get(&url).header("accept", "application/rdap+json");
    // The registry host comes from IANA's bootstrap file (a trusted source).
    req.extra_hosts = vec![host_of(&url)?];
    let (_, v) = send_json(ctx, req, false).await?;
    Ok(AdapterOutput::ok(summarise_rdap(&domain, &v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arxiv_atom_parses() {
        let xml = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
          <entry><id>http://arxiv.org/abs/1706.03762v7</id><published>2017-06-12T17:57:34Z</published>
            <title>Attention Is All
              You Need</title><author><name>Ashish Vaswani</name></author><author><name>Noam Shazeer</name></author>
            <arxiv:doi>10.48550/arXiv.1706.03762</arxiv:doi></entry>
          <entry><id>http://arxiv.org/abs/2</id><title>Second</title></entry></feed>"#;
        let r = parse_arxiv(xml, 1).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["title"], "Attention Is All You Need");
        assert_eq!(r[0]["authors"], json!(["Ashish Vaswani", "Noam Shazeer"]));
        assert_eq!(r[0]["year"], 2017);
        assert_eq!(r[0]["doi"], "10.48550/arXiv.1706.03762");
        assert!(parse_arxiv("<not xml", 1).is_err());
    }

    #[test]
    fn sec_filings_filter_and_links() {
        let sub = json!({"name": "Apple Inc.", "filings": {"recent": {
            "form": ["8-K", "10-K", "10-Q"], "filingDate": ["2026-09-01", "2025-11-01", "2025-08-01"],
            "accessionNumber": ["0000320193-26-000001", "0000320193-25-000106", "0000320193-25-000073"],
            "primaryDocument": ["a.htm", "aapl-20250927.htm", "q.htm"]}}});
        let d = filings_from_submissions("0000320193", &sub, Some("10-k"), 5);
        assert_eq!(d["filings"].as_array().unwrap().len(), 1);
        assert_eq!(d["filings"][0]["url"], "https://www.sec.gov/Archives/edgar/data/320193/000032019325000106/aapl-20250927.htm");
        assert_eq!(filings_from_submissions("0000320193", &sub, None, 2)["filings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rdap_bootstrap_and_summary() {
        let boot = json!({"services": [[["com", "net"], ["https://rdap.verisign.com/com/v1/"]], [["dev"], ["http://insecure.example/", "https://pubapi.registry.google/rdap/"]]]});
        assert_eq!(rdap_base_for(&boot, "stripe.com").as_deref(), Some("https://rdap.verisign.com/com/v1/"));
        assert_eq!(rdap_base_for(&boot, "web.dev").as_deref(), Some("https://pubapi.registry.google/rdap/"), "https only");
        assert!(rdap_base_for(&boot, "x.zz").is_none());
        let v = json!({"events": [{"eventAction": "registration", "eventDate": "1995-09-12"}, {"eventAction": "expiration", "eventDate": "2030-09-11"}],
            "entities": [{"roles": ["registrar"], "vcardArray": ["vcard", [["version", {}, "text", "4.0"], ["fn", {}, "text", "MarkMonitor Inc."]]]}],
            "status": ["client transfer prohibited"], "nameservers": [{"ldhName": "NS1.EXAMPLE.COM"}]});
        let s = summarise_rdap("stripe.com", &v);
        assert_eq!(s["registrar"], "MarkMonitor Inc.");
        assert_eq!(s["expires"], "2030-09-11");
        assert_eq!(s["nameservers"], json!(["NS1.EXAMPLE.COM"]));
    }
}
