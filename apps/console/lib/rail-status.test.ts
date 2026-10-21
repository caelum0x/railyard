// Railyard rail status (one aggregated health check across Mint/Rago/Relay/
// Abacus) -- lets a consumer product or an operator ask ONE endpoint instead
// of probing four hosts directly before routing real traffic at the rail.
import { test } from "node:test";
import assert from "node:assert/strict";

import { checkRailStatus } from "./rail-status.ts";

const NOW = new Date("2026-10-21T12:00:00Z");

test("every service healthy reads overall ok, each with its own latency", async () => {
  const fetchImpl = (async (url: string) => new Response("ok", { status: 200 })) as typeof fetch;
  const result = await checkRailStatus({
    services: [
      { name: "mint", url: "https://mint.ereboslab.com/healthz" },
      { name: "rago", url: "https://rago.ereboslab.com/health" },
    ],
    fetchImpl,
    now: () => NOW,
  });
  assert.equal(result.overall, "ok");
  assert.equal(result.checked_at, NOW.toISOString());
  assert.deepEqual(
    result.services.map((s) => [s.service, s.state, s.http_status]),
    [
      ["mint", "ok", 200],
      ["rago", "ok", 200],
    ],
  );
  for (const s of result.services) assert.equal(typeof s.latency_ms, "number");
});

test("a non-2xx response from one service is down, not thrown; overall is degraded when at least one is still ok", async () => {
  const fetchImpl = (async (url: string) => (url.includes("relay") ? new Response("bad", { status: 500 }) : new Response("ok", { status: 200 }))) as typeof fetch;
  const result = await checkRailStatus({
    services: [
      { name: "mint", url: "https://mint.ereboslab.com/healthz" },
      { name: "relay", url: "https://relay.ereboslab.com/health" },
    ],
    fetchImpl,
    now: () => NOW,
  });
  assert.equal(result.overall, "degraded");
  const relay = result.services.find((s) => s.service === "relay");
  assert.equal(relay?.state, "down");
  assert.equal(relay?.http_status, 500);
});

test("a thrown network error (DNS, connection refused) reads down, not an unhandled rejection", async () => {
  const fetchImpl = (async () => {
    throw new TypeError("fetch failed");
  }) as typeof fetch;
  const result = await checkRailStatus({ services: [{ name: "mint", url: "https://mint.ereboslab.com/healthz" }], fetchImpl, now: () => NOW });
  assert.equal(result.overall, "down");
  assert.deepEqual(
    result.services.map((s) => [s.service, s.state, s.http_status]),
    [["mint", "down", null]],
  );
});

test("a slow service that never answers is down once the timeout elapses, not hung forever", async () => {
  const fetchImpl = ((_url: string, init?: RequestInit) =>
    new Promise<Response>((_resolve, reject) => {
      init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
    })) as typeof fetch;
  const result = await checkRailStatus({ services: [{ name: "mint", url: "https://mint.ereboslab.com/healthz" }], fetchImpl, timeoutMs: 20, now: () => NOW });
  assert.equal(result.services[0].state, "down");
});

test("an unset service url reads not_configured and never calls fetch; it is excluded from the overall verdict", async () => {
  let calls = 0;
  const fetchImpl = (async () => {
    calls++;
    return new Response("ok", { status: 200 });
  }) as typeof fetch;
  const result = await checkRailStatus({
    services: [
      { name: "mint", url: "https://mint.ereboslab.com/healthz" },
      { name: "turnstile", url: null },
    ],
    fetchImpl,
    now: () => NOW,
  });
  assert.equal(calls, 1);
  assert.equal(result.services.find((s) => s.service === "turnstile")?.state, "not_configured");
  assert.equal(result.overall, "ok");
});

test("no service configured at all reads unconfigured, not a false ok", async () => {
  const fetchImpl = (async () => new Response("ok", { status: 200 })) as typeof fetch;
  const result = await checkRailStatus({ services: [{ name: "mint", url: null }], fetchImpl, now: () => NOW });
  assert.equal(result.overall, "unconfigured");
});

test("every configured service down reads overall down", async () => {
  const fetchImpl = (async () => new Response("bad", { status: 503 })) as typeof fetch;
  const result = await checkRailStatus({
    services: [
      { name: "mint", url: "https://mint.ereboslab.com/healthz" },
      { name: "rago", url: "https://rago.ereboslab.com/health" },
    ],
    fetchImpl,
    now: () => NOW,
  });
  assert.equal(result.overall, "down");
});
