//! Mint's connect page (Wave P PY2.3): the only place a seller types a
//! payment key. Plain HTML, no scripts, no third-party assets. The default
//! screen has no key field: Stripe (when switched on) is one button; the
//! seller's own Dodo keys sit under "Advanced".

fn esc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;max-width:560px;margin:48px auto;padding:0 20px;color:#1b1f24;background:#f6f7f9}\
main{background:#fff;border:1px solid #e3e6ea;border-radius:14px;padding:28px;box-shadow:0 8px 24px rgba(0,0,0,.05)}\
h1{font-size:22px;margin:0 0 8px}p{color:#4a5360}label{display:block;margin:14px 0 4px;font-weight:600}\
input[type=password],input[type=text]{width:100%;box-sizing:border-box;padding:10px;border:1px solid #cfd5dc;border-radius:8px}\
.primary{display:inline-block;background:#1b1f24;color:#fff;border:0;border-radius:8px;padding:11px 18px;text-decoration:none;font-weight:600;cursor:pointer}\
details{margin-top:22px}summary{cursor:pointer;color:#4a5360}.error{color:#b42318}code{word-break:break-all;background:#f1f3f5;padding:2px 6px;border-radius:6px}";

fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex\"><title>{}</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>",
        esc(title)
    )
}

/// The connect screen for one token (a 64-character hex id, checked by the caller).
/// `webhook_url` is this seller's Dodo webhook address: Dodo makes the webhook
/// secret only when that endpoint is added, so the address is shown before
/// the secret is asked for (one pass, no second connect link).
pub fn connect(token: &str, stripe: bool, error: Option<&str>, webhook_url: Option<&str>) -> String {
    let err = error.map(|e| format!("<p class=\"error\">{}</p>", esc(e))).unwrap_or_default();
    let default = if stripe {
        format!("<p>Your buyers pay your own account directly. Nothing passes through us.</p><p><a class=\"primary\" href=\"/mint/connect/{}/stripe\">Connect with Stripe</a></p>", esc(token))
    } else {
        "<p>Your buyers pay your own account directly. Nothing passes through us.</p><p>Connecting a Stripe account is not available yet. Use Advanced to connect your own Dodo Payments account.</p>".to_owned()
    };
    let advanced = format!(
        "<details{open}><summary>Advanced: use your own Dodo Payments account</summary>\
         {hook}<form method=\"post\" action=\"/mint/connect/{t}\">\
         <label for=\"api_key\">API key</label><input id=\"api_key\" name=\"api_key\" type=\"password\" autocomplete=\"off\" required>\
         <label for=\"webhook_secret\">Webhook secret</label><input id=\"webhook_secret\" name=\"webhook_secret\" type=\"password\" autocomplete=\"off\" placeholder=\"whsec_...\" required>\
         <label><input type=\"radio\" name=\"mode\" value=\"test\" checked> Test mode</label>\
         <label><input type=\"radio\" name=\"mode\" value=\"live\"> Live mode</label>\
         <p><button class=\"primary\" type=\"submit\">Connect</button></p>\
         <p>We keep the key encrypted and use it only to create your products and checkout pages.</p></form></details>",
        open = if error.is_some() { " open" } else { "" },
        t = esc(token),
        hook = webhook_url
            .map(|u| format!("<p>First, in your Dodo Payments dashboard add a webhook to <code>{}</code> for payments, refunds, disputes and subscriptions. Then paste that webhook's secret below.</p>", esc(u)))
            .unwrap_or_default(),
    );
    shell("Connect your payment account", &format!("<h1>Connect your payment account</h1>{err}{default}{advanced}"))
}

/// After a successful connect.
pub fn connected(provider: &str, webhook_url: Option<&str>, return_url: Option<&str>) -> String {
    let hook = webhook_url
        .map(|u| format!("<p>Your webhook address is <code>{}</code>. Keep it in your {} dashboard for payments, refunds, disputes and subscriptions.</p>", esc(u), esc(provider)))
        .unwrap_or_default();
    let back = return_url
        .filter(|u| u.starts_with("https://"))
        .map(|u| format!("<p><a class=\"primary\" href=\"{}\">Back to your project</a></p>", esc(u)))
        .unwrap_or_else(|| "<p>You can close this tab.</p>".to_owned());
    shell("Connected", &format!("<h1>Connected</h1><p>Your {} account is connected.</p>{hook}{back}", esc(provider)))
}

/// An unknown, used or expired link.
pub fn expired() -> String {
    shell("Link expired", "<h1>This link has expired</h1><p>Start again from your project; each link works once, for 15 minutes.</p>")
}

#[cfg(test)]
mod tests {
    use super::{connect, connected, expired};

    #[test]
    fn the_default_screen_has_no_key_field() {
        for stripe in [true, false] {
            let page = connect(&"a".repeat(64), stripe, None, Some("https://m.example/mint/webhooks/seller/scribase/o.p.e/dodo"));
            let default_part = page.split("<details").next().unwrap();
            assert!(!default_part.contains("type=\"password\""), "no key field on the default screen");
            assert!(page.contains("<details>"), "Advanced is closed by default");
            assert_eq!(page.contains("Connect with Stripe"), stripe);
            assert!(!page.contains("<script"));
            let advanced = page.split("<details").nth(1).unwrap();
            assert!(advanced.find("/mint/webhooks/seller/scribase/o.p.e/dodo").unwrap() < advanced.find("webhook_secret").unwrap(), "the address comes before the secret is asked for");
        }
    }

    #[test]
    fn errors_and_links_are_escaped_and_advanced_opens_on_error() {
        let page = connect(&"b".repeat(64), true, Some("<bad>"), None);
        assert!(page.contains("&lt;bad&gt;") && page.contains("<details open>"));
        let done = connected("Dodo Payments", Some("https://m.example/x?\"<"), Some("javascript:alert(1)"));
        assert!(done.contains("&quot;&lt;") && !done.contains("javascript:"));
        assert!(expired().contains("15 minutes"));
    }
}
