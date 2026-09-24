//! Server-rendered customer portal pages. Every dynamic value is HTML-escaped;
//! notices are fixed messages selected by code (nothing from the query string is
//! reflected).

use chrono::{DateTime, NaiveDate, Utc};
use rago_core::enums::{InvoiceStatus, PaymentStatus, SubscriptionStatus, WalletStatus};
use rago_core::models::{Invoice, Subscription, Wallet};
use rago_core::money::Currency;
use rust_decimal::Decimal;

const TEMPLATE: &str = include_str!("../../assets/portal.html");

/// Escape text for HTML element and attribute context.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `1234` + `USD` → `12.34 USD` (currency exponent aware, integer only).
pub fn money(cents: i64, currency: &str) -> String {
    let cur = Currency::new(currency);
    let exp = cur.exponent();
    let unit = cur.subunit_to_unit().max(1);
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let whole = abs / unit as u64;
    if exp == 0 {
        return format!("{sign}{whole} {}", cur.code());
    }
    let frac = abs % unit as u64;
    format!("{sign}{whole}.{frac:0width$} {}", cur.code(), width = exp as usize)
}

fn date(d: Option<NaiveDate>) -> String {
    d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into())
}

/// Fixed notice text for a notice code.
pub fn notice_text(code: &str) -> Option<(&'static str, bool)> {
    Some(match code {
        "invoice_paid" => ("Payment received. Thank you.", false),
        "payment_pending" => ("Your payment is being processed. This page will show it as paid once it clears.", false),
        "payment_failed" => ("The payment was declined. No money was taken. Try again or use another payment method.", true),
        "payment_unavailable" => ("Online payment is not available for this invoice. Contact the seller to pay it.", true),
        "already_paid" => ("This invoice is already paid.", false),
        "top_up_created" => ("Your top-up invoice was created. The credits are added once it is paid.", false),
        "top_up_invalid" => ("Enter a positive number of credits (at most 1,000,000).", true),
        "wallet_unavailable" => ("This wallet cannot be topped up.", true),
        "subscription_cancelled" => ("Your subscription was cancelled.", false),
        "subscription_unavailable" => ("This subscription can no longer be cancelled.", true),
        "payment_method_unavailable" => ("Payment methods are managed by the seller for this account.", true),
        _ => return None,
    })
}

fn invoice_status_pill(inv: &Invoice) -> String {
    let (label, class) = match (inv.status, inv.payment_status) {
        (InvoiceStatus::Voided, _) => ("Voided", ""),
        (InvoiceStatus::Draft, _) => ("Draft", ""),
        (_, PaymentStatus::Succeeded) => ("Paid", "ok"),
        (_, PaymentStatus::Refunded) => ("Refunded", ""),
        (_, PaymentStatus::Failed) => ("Payment failed", "bad"),
        (_, PaymentStatus::Pending) => ("Due", "warn"),
    };
    format!("<span class=\"pill {class}\">{label}</span>")
}

/// Whether the customer can pay this invoice online now.
pub fn payable(inv: &Invoice) -> bool {
    matches!(inv.status, InvoiceStatus::Finalized)
        && matches!(inv.payment_status, PaymentStatus::Pending | PaymentStatus::Failed)
        && inv.total_amount_cents > 0
}

/// Usage of one subscription in the current period.
pub struct UsageBlock {
    pub subscription_name: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub currency: String,
    pub rows: Vec<(String, Decimal, i64)>,
}

/// Everything the portal home page shows.
pub struct PortalView<'a> {
    pub token: &'a str,
    pub org_name: &'a str,
    pub customer_name: &'a str,
    pub expires_at: DateTime<Utc>,
    pub notice: Option<&'a str>,
    pub invoices: &'a [Invoice],
    pub subscriptions: &'a [(Subscription, String)],
    pub usage: &'a [UsageBlock],
    pub wallets: &'a [Wallet],
    pub can_manage_payment_method: bool,
}

fn section(title: &str, body: String) -> String {
    format!("<section><h2>{}</h2>{body}</section>\n", esc(title))
}

fn invoices_html(v: &PortalView<'_>) -> String {
    if v.invoices.is_empty() {
        return "<p class=\"empty\">No invoices yet.</p>".into();
    }
    let mut rows = String::new();
    for inv in v.invoices.iter().filter(|i| !matches!(i.status, InvoiceStatus::Draft)) {
        let currency = inv.currency.clone().unwrap_or_else(|| "USD".into());
        let pay = if payable(inv) {
            format!(
                "<form class=\"inline\" method=\"post\" action=\"/portal/{t}/invoices/{id}/pay\"><button class=\"primary\" type=\"submit\">Pay</button></form> ",
                t = esc(v.token),
                id = inv.id
            )
        } else {
            String::new()
        };
        rows.push_str(&format!(
            "<tr><td>{num}</td><td>{date}</td><td>{status}</td><td class=\"num\">{amount}</td><td class=\"num\">{pay}<a class=\"btn\" href=\"/portal/{t}/invoices/{id}/download\">PDF</a></td></tr>",
            num = esc(&inv.number),
            date = date(inv.issuing_date),
            status = invoice_status_pill(inv),
            amount = esc(&money(inv.total_amount_cents, &currency)),
            t = esc(v.token),
            id = inv.id,
        ));
    }
    format!(
        "<table><thead><tr><th>Invoice</th><th>Issued</th><th>Status</th><th class=\"num\">Total</th><th class=\"num\"></th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

fn usage_html(v: &PortalView<'_>) -> String {
    if v.usage.is_empty() {
        return "<p class=\"empty\">No usage-based charges this period.</p>".into();
    }
    let mut out = String::new();
    for block in v.usage {
        let total: i64 = block.rows.iter().map(|(_, _, c)| c).sum();
        let mut rows = String::new();
        for (name, units, cents) in &block.rows {
            rows.push_str(&format!(
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                esc(name),
                esc(&units.normalize().to_string()),
                esc(&money(*cents, &block.currency))
            ));
        }
        if rows.is_empty() {
            rows.push_str("<tr><td colspan=\"3\" class=\"muted\">No metered usage yet.</td></tr>");
        }
        out.push_str(&format!(
            "<div class=\"body\"><strong>{}</strong> <span class=\"muted\">{} to {}, so far {}</span></div><table><thead><tr><th>Metric</th><th class=\"num\">Units</th><th class=\"num\">Amount</th></tr></thead><tbody>{rows}</tbody></table>",
            esc(&block.subscription_name),
            block.from.format("%Y-%m-%d"),
            block.to.format("%Y-%m-%d"),
            esc(&money(total, &block.currency)),
        ));
    }
    out
}

fn wallets_html(v: &PortalView<'_>) -> String {
    let active: Vec<&Wallet> = v
        .wallets
        .iter()
        .filter(|w| matches!(w.status, WalletStatus::Active))
        .collect();
    if active.is_empty() {
        return "<p class=\"empty\">No prepaid wallet.</p>".into();
    }
    let mut rows = String::new();
    for w in active {
        rows.push_str(&format!(
            "<tr><td>{name}</td><td class=\"num\">{credits}</td><td class=\"num\">{balance}</td><td class=\"num\"><form class=\"inline\" method=\"post\" action=\"/portal/{t}/wallets/{id}/top_up\"><label class=\"muted\" for=\"c-{id}\">Credits </label><input id=\"c-{id}\" type=\"text\" name=\"credits\" inputmode=\"decimal\" required> <button type=\"submit\">Top up</button></form></td></tr>",
            name = esc(w.name.as_deref().unwrap_or("Wallet")),
            credits = esc(&w.credits_balance.normalize().to_string()),
            balance = esc(&money(w.balance_cents, &w.balance_currency)),
            t = esc(v.token),
            id = w.id,
        ));
    }
    format!(
        "<table><thead><tr><th>Wallet</th><th class=\"num\">Credits</th><th class=\"num\">Balance</th><th class=\"num\"></th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

fn subscriptions_html(v: &PortalView<'_>) -> String {
    let live: Vec<&(Subscription, String)> = v
        .subscriptions
        .iter()
        .filter(|(s, _)| matches!(s.status, SubscriptionStatus::Active | SubscriptionStatus::Pending))
        .collect();
    if live.is_empty() {
        return "<p class=\"empty\">No active subscription.</p>".into();
    }
    let mut rows = String::new();
    for (sub, plan_name) in live {
        let status = match sub.status {
            SubscriptionStatus::Pending => "<span class=\"pill warn\">Starts soon</span>",
            _ => "<span class=\"pill ok\">Active</span>",
        };
        rows.push_str(&format!(
            "<tr><td>{name}</td><td>{status}</td><td>{since}</td><td class=\"num\"><form class=\"inline\" method=\"post\" action=\"/portal/{t}/subscriptions/{id}/cancel\"><button class=\"danger\" type=\"submit\">Cancel subscription</button></form></td></tr>",
            name = esc(sub.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(plan_name)),
            since = sub.started_at.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into()),
            t = esc(v.token),
            id = sub.id,
        ));
    }
    format!(
        "<table><thead><tr><th>Plan</th><th>Status</th><th>Since</th><th class=\"num\"></th></tr></thead><tbody>{rows}</tbody></table><p class=\"body muted\">Cancelling ends the subscription now. Usage up to now is billed on your final invoice.</p>"
    )
}

/// Render the portal home page.
pub fn home(v: &PortalView<'_>) -> String {
    let mut content = String::new();
    content.push_str(&section("Invoices", invoices_html(v)));
    content.push_str(&section("Usage this period", usage_html(v)));
    content.push_str(&section("Prepaid credits", wallets_html(v)));
    content.push_str(&section("Subscriptions", subscriptions_html(v)));
    if v.can_manage_payment_method {
        content.push_str(&section(
            "Payment method",
            format!(
                "<div class=\"body\"><form class=\"inline\" method=\"post\" action=\"/portal/{}/payment_method\"><button type=\"submit\">Update payment method</button></form> <span class=\"muted\">Opens the payment provider's secure page.</span></div>",
                esc(v.token)
            ),
        ));
    }
    let notice = v
        .notice
        .and_then(notice_text)
        .map(|(text, bad)| format!("<div class=\"notice{}\" role=\"status\">{}</div>", if bad { " bad" } else { "" }, esc(text)))
        .unwrap_or_default();
    fill(v.org_name, v.customer_name, v.expires_at, &notice, &content)
}

fn fill(org: &str, customer: &str, expires: DateTime<Utc>, notice: &str, content: &str) -> String {
    TEMPLATE
        .replace("{{title}}", &esc(&format!("{org} billing")))
        .replace("{{org_name}}", &esc(org))
        .replace("{{customer_name}}", &esc(customer))
        .replace("{{expires_at}}", &esc(&expires.format("%Y-%m-%d %H:%M UTC").to_string()))
        .replace("{{notice}}", notice)
        .replace("{{content}}", content)
}

/// The page shown for an invalid, expired or revoked link.
pub fn invalid_link() -> String {
    let content = section(
        "This link is no longer valid",
        "<p class=\"body\">Billing links expire and can be revoked. Ask the company that bills you for a new link.</p>".into(),
    );
    TEMPLATE
        .replace("{{title}}", "Billing link expired")
        .replace("{{org_name}}", "Billing")
        .replace("<p>Billing for {{customer_name}}</p>", "")
        .replace("{{notice}}", "")
        .replace("{{content}}", &content)
        .replace(
            "This page is private to you. Anyone with the link can see it until it expires on {{expires_at}}.",
            "",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_respects_currency_exponent() {
        assert_eq!(money(123_456, "USD"), "1234.56 USD");
        assert_eq!(money(5, "EUR"), "0.05 EUR");
        assert_eq!(money(-250, "TRY"), "-2.50 TRY");
        assert_eq!(money(1_500, "JPY"), "1500 JPY");
    }

    #[test]
    fn escaping_covers_markup_and_quotes() {
        assert_eq!(esc("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn unknown_notice_codes_render_nothing() {
        assert!(notice_text("<script>").is_none());
        assert!(notice_text("invoice_paid").is_some());
    }

    #[test]
    fn invalid_link_page_has_no_placeholders_left() {
        let html = invalid_link();
        assert!(!html.contains("{{"));
        assert!(html.contains("no longer valid"));
    }
}
