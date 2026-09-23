//! OAuth 1.0a request signing (HMAC-SHA256) for NetSuite Token-Based
//! Authentication (TBA): RESTlet script calls and the SuiteQL REST API.
//!
//! NetSuite authenticates every request with an `Authorization: OAuth ...`
//! header signed from the integration record's consumer key/secret and the
//! access token id/secret, with `realm` set to the account id.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The four TBA secrets plus the account realm.
#[derive(Debug, Clone)]
pub struct Oauth1Credentials {
    pub realm: String,
    pub consumer_key: String,
    pub consumer_secret: String,
    pub token_id: String,
    pub token_secret: String,
}

/// RFC 3986 percent-encoding (unreserved characters pass through).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decode a query component (`+` is a space in query strings).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a URL into its base (scheme://host/path) and decoded query pairs.
fn split_url(url: &str) -> (String, Vec<(String, String)>) {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, q),
        None => (url, ""),
    };
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect();
    (base.to_string(), pairs)
}

/// Build the `Authorization` header for `method url` with an explicit nonce
/// and timestamp (deterministic; see [`authorization_header`]).
pub fn authorization_header_with(
    creds: &Oauth1Credentials,
    method: &str,
    url: &str,
    nonce: &str,
    timestamp: i64,
) -> String {
    let (base_url, query) = split_url(url);
    let timestamp = timestamp.to_string();
    let oauth_params: Vec<(String, String)> = vec![
        ("oauth_consumer_key".into(), creds.consumer_key.clone()),
        ("oauth_nonce".into(), nonce.to_string()),
        ("oauth_signature_method".into(), "HMAC-SHA256".into()),
        ("oauth_timestamp".into(), timestamp),
        ("oauth_token".into(), creds.token_id.clone()),
        ("oauth_version".into(), "1.0".into()),
    ];
    let mut all: Vec<(String, String)> = oauth_params
        .iter()
        .cloned()
        .chain(query)
        .map(|(k, v)| (encode(&k), encode(&v)))
        .collect();
    all.sort();
    let param_string = all.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let base_string = format!(
        "{}&{}&{}",
        method.to_ascii_uppercase(),
        encode(&base_url),
        encode(&param_string)
    );
    let key = format!("{}&{}", encode(&creds.consumer_secret), encode(&creds.token_secret));
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(base_string.as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    let mut header = format!("OAuth realm=\"{}\"", encode(&creds.realm));
    for (k, v) in &oauth_params {
        header.push_str(&format!(", {}=\"{}\"", k, encode(v)));
    }
    header.push_str(&format!(", oauth_signature=\"{}\"", encode(&signature)));
    header
}

/// Build a freshly-nonced `Authorization` header for `method url`.
pub fn authorization_header(creds: &Oauth1Credentials, method: &str, url: &str) -> String {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    authorization_header_with(creds, method, url, &nonce, chrono::Utc::now().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Oauth1Credentials {
        Oauth1Credentials {
            realm: "1234567_SB1".into(),
            consumer_key: "ck".into(),
            consumer_secret: "cs".into(),
            token_id: "tk".into(),
            token_secret: "ts".into(),
        }
    }

    #[test]
    fn header_is_deterministic_and_carries_realm() {
        let url = "https://1234567-sb1.restlets.api.netsuite.com/app/site/hosting/restlet.nl?script=1&deploy=1";
        let a = authorization_header_with(&creds(), "POST", url, "n1", 1_700_000_000);
        let b = authorization_header_with(&creds(), "POST", url, "n1", 1_700_000_000);
        assert_eq!(a, b);
        assert!(a.starts_with("OAuth realm=\"1234567_SB1\""));
        assert!(a.contains("oauth_signature_method=\"HMAC-SHA256\""));
        let c = authorization_header_with(&creds(), "POST", url, "n2", 1_700_000_000);
        assert_ne!(a, c);
    }

    #[test]
    fn query_params_are_part_of_the_signature() {
        let a = authorization_header_with(&creds(), "GET", "https://x.test/p?script=1", "n", 1);
        let b = authorization_header_with(&creds(), "GET", "https://x.test/p?script=2", "n", 1);
        assert_ne!(a, b);
    }
}
