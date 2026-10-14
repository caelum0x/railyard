#!/usr/bin/env node
// Rithub (Gitea) -> railyard billing bridge.
//
// Receives Gitea webhooks (apps/gitea — go-gitea/gitea, MIT — Railyard's
// code-hosting service, branded "Rithub"), verifies the HMAC-SHA256
// signature, maps forge events to Rago usage events (Lago-compatible
// billing), and — when a paid boundary is crossed (e.g. a new private repo
// or an added org seat) — records the metered usage that Rago later
// aggregates into an invoice that Mint settles in AIT.
//
// Forked near-verbatim from apps/rithub-forge/integrations/railyard (an
// earlier, uncommitted Forgejo experiment): Gitea and Forgejo share the same
// webhook payload shape (Forgejo is a soft-fork of Gitea), so this bridge
// already accepted X-Gitea-* headers as a fallback — now promoted to the
// primary case. Forgejo header names stay as a secondary fallback so a
// delivery from the untouched rithub-forge experiment (if ever registered)
// would still be accepted.
//
// This is intentionally dependency-free (Node stdlib only) so it runs without
// an install/build step. It is a real, runnable receiver — not a mock. Point a
// Gitea repo/org webhook at http://<host>:<PORT>/gitea/webhook.
//
// Env:
//   PORT                  listen port (default 8091)
//   RITHUB_WEBHOOK_SECRET  shared secret configured on the rithub webhook
//   RAGO_WEBHOOK_SECRET    shared HMAC key Rago signs its OUTBOUND webhooks with
//                          (org.hmac_key). REQUIRED to accept /rago/invoice —
//                          without it settlement is rejected (fail closed).
//   RAGO_BASE_URL         e.g. http://127.0.0.1:8080
//   RAGO_API_KEY          Bearer key for Rago's /api/v1 routes
//   MINT_BASE_URL         e.g. http://127.0.0.1:8080 (Mint settlement, optional)
//   MINT_API_KEY          Bearer key for Mint
//   DRY_RUN=1             log the mapped billing calls instead of sending them

import http from 'node:http';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const PORT = Number(process.env.PORT || 8091);
const SECRET = process.env.RITHUB_WEBHOOK_SECRET || '';
const RAGO_BASE_URL = process.env.RAGO_BASE_URL || 'http://127.0.0.1:8080';
const RAGO_API_KEY = process.env.RAGO_API_KEY || '';
const MINT_BASE_URL = process.env.MINT_BASE_URL || '';
const MINT_API_KEY = process.env.MINT_API_KEY || '';
const DRY_RUN = process.env.DRY_RUN === '1';

// Billable-metric codes registered in Rago (billable_metrics). Kept here so the
// mapping between a forge action and what the customer is billed for is explicit.
const METRIC = {
  PRIVATE_REPO: 'rithub_private_repo',
  SEAT: 'rithub_seat',
  CI_PUSH: 'rithub_ci_push',
};

// Constant-time compare of two strings (returns false on length mismatch
// without leaking via early return timing beyond the length check).
function safeEqual(a, b) {
  const ba = Buffer.from(String(a));
  const bb = Buffer.from(String(b));
  return ba.length === bb.length && crypto.timingSafeEqual(ba, bb);
}

// Verify Gitea's HMAC on the FORGE webhook route. Gitea sends
// X-Gitea-Signature (hex sha256 of the raw body keyed by the webhook secret);
// a Forgejo delivery (soft-fork, same shape) sends X-Forgejo-Signature instead.
// Constant-time compare.
// This route only RECORDS metered usage (no funds move), so an unset secret is
// permitted in dev; the fund-moving settlement route below fails closed instead.
export function verify(rawBody, headerSig, secret = SECRET) {
  if (!secret) return true; // no secret configured -> accept (dev only)
  if (!headerSig) return false;
  const mac = crypto.createHmac('sha256', secret).update(rawBody).digest('hex');
  return safeEqual(mac, headerSig);
}

// Verify RAGO's OWN outbound-webhook signature on the settlement route.
// Rago is Lago-compatible and signs deliveries as
// base64(HMAC-SHA256(org.hmac_key, raw_body)) in the X-Lago-Signature header
// (see rago-integrations/src/webhooks.rs: Webhook::hmac_signature/headers).
// FAIL CLOSED: with no configured secret we REJECT — /rago/invoice moves real
// funds via Mint, so we never accept-all. Reads the secret dynamically so it is
// always evaluated against current configuration.
export function verifyRago(rawBody, headerSig, secret = process.env.RAGO_WEBHOOK_SECRET || '') {
  if (!secret) return false; // fail closed — settlement requires a shared secret
  if (!headerSig) return false;
  const mac = crypto.createHmac('sha256', secret).update(rawBody).digest('base64');
  return safeEqual(mac, headerSig);
}

// Build a DETERMINISTIC transaction_id so a redelivered webhook produces the
// SAME id and Rago's event dedup collapses it (no double-billing). The primary
// key is Gitea's per-delivery UUID (X-Gitea-Delivery / X-Forgejo-Delivery /
// X-GitHub-Delivery), which is stable across redeliveries of the same delivery.
// A single delivery can map to multiple metered events, so the metric `code` is
// mixed in to keep each event within a delivery distinct yet reproducible.
// When no delivery id is available we fall back to a stable hash of the delivery
// identity (event type + repo + action + resource id), never a random UUID.
export function stableTxnId(deliveryId, code, eventType, payload) {
  if (deliveryId) return `rithub_${deliveryId}_${code}`;
  const material = [
    eventType,
    code,
    payload?.repository?.full_name ?? '',
    payload?.repository?.id ?? '',
    payload?.action ?? '',
    // disambiguate distinct pushes to the same repo in the fallback path
    payload?.after ?? payload?.head_commit?.id ?? '',
  ].join('|');
  const h = crypto.createHash('sha256').update(material).digest('hex').slice(0, 40);
  return `rithub_${h}`;
}

// Map one forge webhook to zero or more Rago usage events.
// org -> Rago external_customer_id ; org subscription -> external_subscription_id.
// deliveryId is Gitea's webhook delivery UUID (stable across redeliveries).
export function toRagoEvents(eventType, payload, deliveryId = '') {
  const now = Math.floor(Date.now() / 1000);
  const org =
    payload?.repository?.owner?.username ||
    payload?.organization?.username ||
    payload?.repository?.owner?.login ||
    'unknown_org';
  const sub = `sub_${org}`;
  const base = (code, properties) => ({
    transaction_id: stableTxnId(deliveryId, code, eventType, payload),
    external_subscription_id: sub,
    external_customer_id: org,
    code,
    timestamp: now,
    properties,
  });

  switch (eventType) {
    case 'repository':
      // a new PRIVATE repository is the paid boundary
      if (payload?.action === 'created' && payload?.repository?.private) {
        return [base(METRIC.PRIVATE_REPO, { repo: payload.repository.full_name, op: '+1' })];
      }
      if (payload?.action === 'deleted' && payload?.repository?.private) {
        return [base(METRIC.PRIVATE_REPO, { repo: payload.repository.full_name, op: '-1' })];
      }
      return [];
    case 'member': // org seat added/removed
      return [base(METRIC.SEAT, { user: payload?.user?.username, action: payload?.action })];
    case 'push': // meter CI/compute per push
      return [base(METRIC.CI_PUSH, {
        repo: payload?.repository?.full_name,
        commits: Array.isArray(payload?.commits) ? payload.commits.length : 0,
      })];
    default:
      return [];
  }
}

async function postJSON(url, headers, body) {
  if (DRY_RUN) { console.log('[dry-run] POST', url, JSON.stringify(body)); return { ok: true, status: 0 }; }
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', ...headers },
    body: JSON.stringify(body),
  });
  return { ok: res.ok, status: res.status, text: await res.text().catch(() => '') };
}

async function forwardToRago(ragoEvent) {
  // Rago is Lago-compatible: POST /api/v1/events { event: {...} } with Bearer auth.
  return postJSON(`${RAGO_BASE_URL}/api/v1/events`, { Authorization: `Bearer ${RAGO_API_KEY}` }, { event: ragoEvent });
}

// Lago/Rago outbound webhook types that represent a FINALIZED, billable invoice
// worth settling. Everything else (drafts, payment-status/failure notices,
// voided invoices) must NOT move funds — settlement is idempotent per invoice
// but must also be triggered only by the right lifecycle event.
const SETTLEABLE_WEBHOOK_TYPES = new Set([
  'invoice.created',    // Lago fires this when a finalized invoice is created
  'invoice.generated',
  'invoice.finalized',
]);

// Decide whether a Rago invoice webhook should trigger a real Mint transfer.
// Fail closed: only a known finalized/billable webhook_type with a positive,
// non-draft total is settleable. Pure + deterministic so it is unit-testable.
export function isSettleableInvoice(webhookType, invoice) {
  if (!SETTLEABLE_WEBHOOK_TYPES.has(String(webhookType || ''))) return false;
  const status = String(invoice?.status ?? '').toLowerCase();
  if (status === 'draft' || status === 'voided') return false;
  const cents = Number(invoice?.total_amount_cents ?? invoice?.total ?? 0);
  return Number.isFinite(cents) && cents > 0;
}

// Resolve the Mint SOURCE account for the org being billed. Multi-tenant safe:
// each org settles from ITS OWN account, resolved from the invoice's
// external_customer_id, so one org's invoice can never drain another org's (or
// a shared global) account. Config:
//   MINT_ORG_ACCT_MAP  JSON object {"<external_customer_id>": "<mint acct id>"}
//   MINT_ORG_ACCT      legacy single-tenant account, honored ONLY for the org
//                      named in MINT_ORG_ID (never as a catch-all default).
// Returns undefined when the org has no mapping so settlement fails closed and
// the WRONG account is never debited. Pure + deterministic for unit tests.
export function resolveOrgAccount(externalCustomerId, env = process.env) {
  const id = String(externalCustomerId || '');
  if (!id) return undefined;
  const rawMap = env.MINT_ORG_ACCT_MAP;
  if (rawMap) {
    let map;
    try { map = JSON.parse(rawMap); } catch { return undefined; }
    if (map && typeof map === 'object' && map[id]) return String(map[id]);
  }
  if (env.MINT_ORG_ACCT && env.MINT_ORG_ID && env.MINT_ORG_ID === id) {
    return String(env.MINT_ORG_ACCT);
  }
  return undefined;
}

// When Rago finalizes an invoice it fires invoice.created back to /rago/invoice;
// we settle it in Mint by transferring AIT from the org's Mint account to the
// platform treasury. Mint auth is X-API-Key; the settlement primitive is
// POST /v1/transfers { from, to, amount_micros, idem_key, ref }.
// The org->Mint-account mapping resolves the SOURCE account from the invoice's
// external_customer_id (multi-tenant safe); an unmapped org fails closed.
async function settleInvoiceViaMint(invoice) {
  if (!MINT_BASE_URL) return { ok: false, status: 0, text: 'mint not configured' };
  const externalCustomerId = invoice.external_customer_id ?? invoice.customer?.external_id;
  const from = resolveOrgAccount(externalCustomerId);
  const to = process.env.MINT_PLATFORM_ACCT;
  if (!from) return { ok: false, status: 0, text: `no Mint account mapping for org '${externalCustomerId ?? ''}' (refusing to settle from a default account)` };
  if (!to) return { ok: false, status: 0, text: 'MINT_PLATFORM_ACCT unset' };
  // Rago totals are integer cents; 1 cent = 10_000 micro-USD.
  const cents = Number(invoice.total_amount_cents ?? invoice.total ?? 0);
  const amount_micros = Math.round(cents * 10_000);
  // Never move zero/negative funds even if reached directly (defense in depth).
  if (!Number.isFinite(amount_micros) || amount_micros <= 0) {
    return { ok: false, status: 0, text: `non-positive settlement amount (${amount_micros} micros)` };
  }
  return postJSON(`${MINT_BASE_URL}/v1/transfers`, { 'X-API-Key': MINT_API_KEY }, {
    from, to, amount_micros,
    idem_key: `rithub-invoice-${invoice.lago_id || invoice.id || invoice.number}`,
    ref: `rithub invoice ${invoice.number || invoice.id}`,
  });
}

const server = http.createServer((req, res) => {
  if (req.method !== 'POST') { res.writeHead(405); return res.end('method not allowed'); }
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', async () => {
    const raw = Buffer.concat(chunks).toString('utf8');

    // Route A: forge -> Rago usage. Verify the FORGE HMAC (records usage only).
    // Also answered at the legacy /forgejo/webhook path so a webhook left over
    // from the untouched rithub-forge experiment keeps working unmodified.
    if (req.url === '/gitea/webhook' || req.url === '/forgejo/webhook') {
      const sig = req.headers['x-gitea-signature'] || req.headers['x-forgejo-signature'];
      if (!verify(raw, sig)) { res.writeHead(401); return res.end('bad signature'); }
      let payload; try { payload = JSON.parse(raw); } catch { res.writeHead(400); return res.end('bad json'); }
      const eventType = req.headers['x-gitea-event'] || req.headers['x-forgejo-event'] || 'unknown';
      // Stable per-delivery UUID (same value on redelivery) drives idempotency.
      const deliveryId =
        req.headers['x-gitea-delivery'] ||
        req.headers['x-forgejo-delivery'] ||
        req.headers['x-github-delivery'] ||
        '';
      const events = toRagoEvents(String(eventType), payload, String(deliveryId));
      const results = [];
      for (const ev of events) results.push({ code: ev.code, ...(await forwardToRago(ev)) });
      res.writeHead(200, { 'Content-Type': 'application/json' });
      return res.end(JSON.stringify({ mapped: events.length, results }));
    }

    // Route B: Rago invoice -> Mint settlement. This MOVES FUNDS, so it must
    // carry Rago's OWN webhook signature (X-Lago-Signature). Fails closed when
    // RAGO_WEBHOOK_SECRET is unset — never accept-all.
    if (req.url === '/rago/invoice') {
      const sig = req.headers['x-lago-signature'];
      if (!verifyRago(raw, sig)) { res.writeHead(401); return res.end('bad rago signature'); }
      let payload; try { payload = JSON.parse(raw); } catch { res.writeHead(400); return res.end('bad json'); }
      const invoice = payload?.invoice || payload;
      // Only settle FINALIZED, positive-total invoices. A drafted / voided /
      // payment_failure / zero-total delivery must never move funds.
      if (!isSettleableInvoice(payload?.webhook_type, invoice)) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        return res.end(JSON.stringify({ settled: false, skipped: true, webhook_type: payload?.webhook_type ?? null }));
      }
      const r = await settleInvoiceViaMint(invoice);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      return res.end(JSON.stringify({ settled: r.ok, status: r.status }));
    }

    res.writeHead(404); res.end('not found');
  });
});

// Only bind a port when run directly (so unit tests can import the pure
// functions above without starting a listener).
const isMain = process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  server.listen(PORT, () => console.log(`rithub(gitea)->railyard bridge on :${PORT} (rago=${RAGO_BASE_URL}, dryRun=${DRY_RUN})`));
}

export { server, settleInvoiceViaMint, SETTLEABLE_WEBHOOK_TYPES };
