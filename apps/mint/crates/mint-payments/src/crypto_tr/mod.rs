//! Crypto + Turkish on-ramps: USDC (on-chain / Coinbase Commerce) and Iyzico.
//!
//! - [`usdc`] settles in [`mint_reserve::Asset::Usdc`]; the webhook path is a
//!   Coinbase Commerce `X-CC-Webhook-Signature` HMAC, and the pure-chain path
//!   re-confirms a tx via JSON-RPC (`confirm_deposit`).
//! - [`iyzico`] settles in `Usd`; the 3DS callback is re-verified against
//!   `/payment/detail` with an IYZWSv2 auth header before crediting.

pub mod iyzico;
pub mod usdc;

pub use iyzico::IyzicoRamp;
pub use usdc::UsdcRamp;

/// Render a microdollar amount as a fixed 2-decimal currency string
/// (`25_000_000 µ$` → `"25.00"`) WITHOUT floating point. Used to build the
/// `local_price` (Coinbase Commerce) / `price` (Iyzico) fields when creating a
/// checkout/charge. Negative amounts are clamped to `0.00` (checkouts are for
/// positive on-ramp deposits only).
pub(crate) fn micros_to_dollars(micros: i64) -> String {
    let micros = micros.max(0);
    format!(
        "{}.{:02}",
        micros / 1_000_000,
        (micros % 1_000_000) / 10_000
    )
}

/// Parse an `application/x-www-form-urlencoded` body into `(key, value)` pairs,
/// percent-decoding both sides (`+` → space). Used by the Iyzico 3DS callback.
pub(crate) fn parse_form(raw: &[u8]) -> Vec<(String, String)> {
    let s = String::from_utf8_lossy(raw);
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let mut kv = pair.splitn(2, '=');
            let k = percent_decode(kv.next().unwrap_or(""));
            let v = percent_decode(kv.next().unwrap_or(""));
            (k, v)
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// In-crate test doubles for the `usdc` / `iyzico` unit tests: a routing fake
/// [`HttpTransport`] (canned JSON matched by URL substring, with the last POST
/// body captured for request-shape assertions) + signing/header helpers. NO
/// network, NO keys — the sandbox has no provider accounts, so every adapter is
/// exercised against fake signed payloads here (live at deploy with real keys).
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::http::{HttpResponse, HttpTransport, TransportError};
    use crate::verify::hmac_sha256_raw;

    /// A fake transport returning canned responses keyed by URL substring, with
    /// the most recent POST body recorded so tests can assert the request shape.
    pub struct FakeTransport {
        routes: Vec<(String, u16, Vec<u8>)>,
        fail: bool,
        last_body: Mutex<Vec<u8>>,
    }

    impl FakeTransport {
        pub fn new() -> Self {
            FakeTransport {
                routes: Vec::new(),
                fail: false,
                last_body: Mutex::new(Vec::new()),
            }
        }

        /// A transport whose every call fails at the transport level (→ outage).
        pub fn failing() -> Arc<dyn HttpTransport> {
            Arc::new(FakeTransport {
                routes: Vec::new(),
                fail: true,
                last_body: Mutex::new(Vec::new()),
            })
        }

        /// Add a canned JSON response for any URL containing `pat`.
        pub fn route(mut self, pat: &str, status: u16, body: serde_json::Value) -> Self {
            self.routes
                .push((pat.to_string(), status, serde_json::to_vec(&body).unwrap()));
            self
        }

        pub fn arc(self) -> Arc<FakeTransport> {
            Arc::new(self)
        }

        /// The raw bytes of the most recent POST (for request-shape assertions).
        pub fn last_post_body(&self) -> Vec<u8> {
            self.last_body.lock().unwrap().clone()
        }

        fn resolve(&self, url: &str) -> Result<HttpResponse, TransportError> {
            if self.fail {
                return Err(TransportError("simulated outage".into()));
            }
            for (pat, status, body) in &self.routes {
                if url.contains(pat.as_str()) {
                    return Ok(HttpResponse {
                        status: *status,
                        body: body.clone(),
                    });
                }
            }
            Ok(HttpResponse {
                status: 200,
                body: b"{}".to_vec(),
            })
        }
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn get(
            &self,
            url: &str,
            _headers: &[(String, String)],
        ) -> Result<HttpResponse, TransportError> {
            self.resolve(url)
        }

        async fn post_json(
            &self,
            url: &str,
            _headers: &[(String, String)],
            body: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            *self.last_body.lock().unwrap() = body.to_vec();
            self.resolve(url)
        }
    }

    /// A case-insensitive header bag implementing [`crate::http::Headers`].
    pub fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Coinbase Commerce `hex(HMAC-SHA256(secret, raw_body))` over the RAW body.
    pub fn hmac_hex(secret: &[u8], body: &[u8]) -> String {
        hex::encode(hmac_sha256_raw(secret, body))
    }
}
