// Dodo webhook -> entitlements (Standard Webhooks). Framework-free so the
// route handler stays a few lines and the whole path is tested end to end.
//
// Order: verify signature and timestamp -> brand check -> record the message
// id (a replay is acknowledged and ignored) -> resolve the org -> ladder ->
// wallet effects -> save. If an effect fails, the message id is released and
// a 502 asks Dodo to retry.

import type { Dodo } from "./dodo.ts";
import { verifiedMetadataOrg } from "./dodo.ts";
import { applyLadder, type LadderEvent } from "./ladder.ts";
import type { Repo } from "./repo.ts";
import { verify } from "./standard-webhooks.ts";
import { emptyEntitlement, type BillingEvent } from "./types.ts";

export interface WebhookDeps {
  repo: Repo;
  dodo: Pick<Dodo, "wallet">;
  webhookSecret: string;
  sessionSecret: string;
  brandId: string | null;
  now: () => Date;
}

export interface WebhookResult {
  status: number;
  body: Record<string, unknown>;
}

type Data = Record<string, unknown>;

const str = (v: unknown): string | null => (typeof v === "string" && v.length > 0 ? v : null);

function customerOf(data: Data): string | null {
  const c = data.customer as Data | undefined;
  return str(c?.customer_id) ?? str(data.customer_id);
}

function intervalOf(data: Data, fromMeta: string | null): "month" | "year" {
  if (fromMeta === "year" || fromMeta === "month") return fromMeta;
  const f = String(data.payment_frequency_interval ?? "").toLowerCase();
  return f.startsWith("year") ? "year" : "month";
}

async function toLadder(type: string, data: Data, orgMeta: { kind: string; interval: string | null } | null, at: Date): Promise<LadderEvent | null> {
  switch (type) {
    case "payment.succeeded":
      return orgMeta?.kind === "card_check" && str(data.payment_id)
        ? { kind: "card_check_paid", paymentId: String(data.payment_id), customerId: customerOf(data), at }
        : null;
    case "subscription.active":
    case "subscription.renewed":
    case "subscription.plan_changed":
      return str(data.subscription_id)
        ? { kind: "subscription_active", subscriptionId: String(data.subscription_id), customerId: customerOf(data), interval: intervalOf(data, orgMeta?.interval ?? null), at }
        : null;
    case "subscription.paused":
      return { kind: "subscription_paused", at };
    case "subscription.unpaused":
      return { kind: "subscription_resumed", at };
    case "subscription.updated":
      return typeof data.cancel_at_next_billing_date === "boolean" ? { kind: "subscription_cancel_scheduled", on: data.cancel_at_next_billing_date, at } : null;
    case "subscription.cancelled":
    case "subscription.expired":
    case "subscription.failed":
      return { kind: "subscription_ended", at };
    default:
      return null;
  }
}

export async function handleDodoWebhook(
  deps: WebhookDeps,
  headers: { id: string | null; timestamp: string | null; signature: string | null },
  rawBody: string,
): Promise<WebhookResult> {
  const now = deps.now();
  if (!(await verify(deps.webhookSecret, headers, rawBody, Math.floor(now.getTime() / 1000)))) {
    return { status: 401, body: { error: "invalid_signature" } };
  }
  let event: { type?: unknown; data?: unknown };
  try {
    event = JSON.parse(rawBody);
  } catch {
    return { status: 400, body: { error: "invalid_body" } };
  }
  const type = str(event.type);
  const data = (event.data ?? null) as Data | null;
  if (!type || !data || typeof data !== "object") return { status: 400, body: { error: "invalid_body" } };
  const webhookId = `dodo:${headers.id}`;
  if (deps.brandId && str(data.brand_id) && data.brand_id !== deps.brandId) {
    return { status: 200, body: { received: true, ignored: true, reason: "other_brand" } };
  }

  const meta = await verifiedMetadataOrg(deps.sessionSecret, data.metadata);
  let orgId = meta?.orgId ?? null;
  let kind = meta?.kind ?? null;
  const paymentId = str(data.payment_id);
  let ladder = await toLadder(type, data, meta, now);
  if (type === "refund.succeeded" && paymentId) {
    const paid = await deps.repo.findBillingEventByPayment(paymentId, "card_check");
    if (paid?.org_id) {
      orgId = paid.org_id;
      kind = "card_check_refund";
      ladder = { kind: "card_check_refunded", paymentId, at: now };
    }
  }

  const record: BillingEvent = {
    webhook_id: webhookId,
    org_id: orgId,
    event_type: type,
    payment_id: paymentId,
    kind: type === "payment.succeeded" ? kind : kind === "card_check_refund" ? kind : null,
    amount_cents: typeof data.total_amount === "number" ? data.total_amount : null,
    outcome: !orgId ? "ignored_unknown_org" : ladder ? "applied" : "recorded",
  };
  const fresh = await deps.repo.recordBillingEvent(record);
  if (!fresh) return { status: 200, body: { received: true, replayed: true } };
  if (!orgId || !ladder) return { status: 200, body: { received: true, applied: false, reason: record.outcome } };

  const current = (await deps.repo.getEntitlement(orgId)) ?? emptyEntitlement(orgId, now);
  const { ent, effects } = applyLadder(current, ladder);
  try {
    for (const e of effects) {
      await deps.dodo.wallet(e.customerId, { type: e.kind === "wallet_credit" ? "credit" : "debit", cents: e.cents, idempotencyKey: e.idempotencyKey, reason: e.reason });
    }
    await deps.repo.saveEntitlement(ent);
  } catch (err) {
    await deps.repo.releaseBillingEvent(webhookId);
    console.error(`billing webhook ${webhookId} (${type}) for org ${orgId} failed: ${(err as Error).message}`);
    return { status: 502, body: { error: "retry" } };
  }
  return { status: 200, body: { received: true, applied: true, org_id: orgId, plan: ent.plan, status: ent.status } };
}
