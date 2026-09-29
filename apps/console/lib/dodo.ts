// Dodo Payments client for the console (brand "railyard"). Test mode by
// default: the live host is refused unless the owner sets RAILYARD_DODO_LIVE=1.

import { DODO_LIVE_BASE } from "./config.ts";
import { b64url, constantTimeEqual, hmacSha256 } from "./crypto.ts";

type Fetch = typeof fetch;

export class DodoError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export interface CheckoutInput {
  productId: string;
  email: string;
  name?: string;
  metadata: Record<string, string>;
  returnUrl: string;
}

export class Dodo {
  base: string;
  apiKey: string;
  fetchImpl: Fetch;

  constructor(opts: { base: string; apiKey: string; allowLive: boolean; fetchImpl?: Fetch }) {
    if (opts.base.replace(/\/$/, "") === DODO_LIVE_BASE && !opts.allowLive) throw new Error("live Dodo calls are off (test mode)");
    this.base = opts.base.replace(/\/$/, "");
    this.apiKey = opts.apiKey;
    this.fetchImpl = opts.fetchImpl ?? fetch;
  }

  async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await this.fetchImpl(`${this.base}${path}`, {
      method,
      headers: { authorization: `Bearer ${this.apiKey}`, "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    if (!res.ok) throw new DodoError(`payment provider ${method} ${path.split("/").slice(0, 2).join("/")} failed (${res.status})`, res.status);
    return (text ? JSON.parse(text) : {}) as T;
  }

  async createCheckout(input: CheckoutInput): Promise<{ checkoutUrl: string; sessionId: string | null }> {
    const out = await this.call<{ checkout_url?: string; session_id?: string }>("POST", "/checkouts", {
      product_cart: [{ product_id: input.productId, quantity: 1 }],
      customer: { email: input.email, name: input.name ?? input.email.split("@")[0] },
      minimal_address: true,
      metadata: input.metadata,
      return_url: input.returnUrl,
    });
    if (!out.checkout_url || !/^https:\/\//.test(out.checkout_url)) throw new DodoError("checkout did not return a URL", 502);
    return { checkoutUrl: out.checkout_url, sessionId: out.session_id ?? null };
  }

  async wallet(customerId: string, entry: { type: "credit" | "debit"; cents: number; idempotencyKey: string; reason: string }) {
    return this.call("POST", `/customers/${encodeURIComponent(customerId)}/wallets/ledger-entries`, {
      amount: entry.cents,
      currency: "USD",
      entry_type: entry.type,
      idempotency_key: entry.idempotencyKey,
      reason: entry.reason,
    });
  }

  /** Pause or resume: Dodo requires `status` on its own in the body. */
  async setPaused(subscriptionId: string, paused: boolean) {
    return this.call("PATCH", `/subscriptions/${encodeURIComponent(subscriptionId)}`, { status: paused ? "paused" : "active" });
  }

  /** One-click cancel keeps the paid time: it ends at the next billing date. */
  async cancelAtPeriodEnd(subscriptionId: string) {
    return this.call("PATCH", `/subscriptions/${encodeURIComponent(subscriptionId)}`, { cancel_at_next_billing_date: true });
  }
}

// Checkout metadata is signed so a payment link with made-up metadata cannot
// name an org (anyone can pay for a product with metadata of their choosing).
export async function signMetadata(secret: string, orgId: string, kind: string, requestId: string): Promise<string> {
  return b64url(await hmacSha256(`${secret}:checkout-metadata`, `${orgId}.${kind}.${requestId}`));
}

export async function verifiedMetadataOrg(secret: string, metadata: unknown): Promise<{ orgId: string; kind: string; interval: string | null } | null> {
  if (!metadata || typeof metadata !== "object") return null;
  const m = metadata as Record<string, unknown>;
  const { org_id: orgId, kind, request_id: requestId, sig, service } = m;
  if (service !== "railyard" || typeof orgId !== "string" || typeof kind !== "string" || typeof requestId !== "string" || typeof sig !== "string") return null;
  const expected = await signMetadata(secret, orgId, kind, requestId);
  if (!constantTimeEqual(sig, expected)) return null;
  return { orgId, kind, interval: typeof m.interval === "string" ? m.interval : null };
}
