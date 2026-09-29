// Shared fakes: a Dodo test-mode API and a Scribase Auth server, both in memory.

import { sign } from "../lib/standard-webhooks.ts";

export const WEBHOOK_SECRET = `whsec_${Buffer.from("railyard-test-webhook-secret-32b").toString("base64")}`;
export const SESSION_SECRET = "test-session-secret-not-real-0123456789";

export function fakeDodo() {
  const calls: { method: string; url: string; body: any }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    const body = init.body ? JSON.parse(String(init.body)) : null;
    calls.push({ method: String(init.method), url, body });
    if (url.startsWith("https://live.")) throw new Error("live Dodo call attempted");
    if (url.endsWith("/checkouts")) {
      return new Response(JSON.stringify({ session_id: `cks_${calls.length}`, checkout_url: `https://test.checkout.dodopayments.com/session/cks_${calls.length}` }), { status: 200 });
    }
    return new Response(JSON.stringify({ balance: 100, currency: "USD" }), { status: 200 });
  }) as typeof fetch;
  return { calls, fetchImpl };
}

export function fakeScribaseAuth(user = { id: "7f1c9a70-0000-4000-8000-000000000001", email: "Founder@Example.com" }) {
  const fetchImpl = (async (url: string, init: RequestInit) => {
    if (url.includes("/auth/v1/token?grant_type=pkce")) {
      const b = JSON.parse(String(init.body));
      if (b.auth_code !== "good-code" || !b.code_verifier) return new Response("{}", { status: 400 });
      return new Response(JSON.stringify({ access_token: "at-1", user: { id: "spoofed" } }), { status: 200 });
    }
    if (url.endsWith("/auth/v1/user")) {
      const h = new Headers(init.headers);
      if (h.get("authorization") !== "Bearer at-1") return new Response("{}", { status: 401 });
      return new Response(JSON.stringify(user), { status: 200 });
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;
  return { fetchImpl, user };
}

let counter = 0;
export async function signedWebhook(type: string, data: Record<string, unknown>, at: Date, id = `msg_${++counter}`) {
  const body = JSON.stringify({ business_id: "bus_test", type, timestamp: at.toISOString(), data });
  const ts = String(Math.floor(at.getTime() / 1000));
  return { headers: { id, timestamp: ts, signature: await sign(WEBHOOK_SECRET, id, ts, body) }, body };
}
