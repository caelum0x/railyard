// An agent failure is its own record: agent, model, and error name.
// A page view is not one. A generic custom event is not one.
// A prompt or a secret is never kept.
import assert from "node:assert/strict";
import { test } from "node:test";
import { agentFailureRecord, faultlineIssue } from "../src/agent-failure.mjs";
import { parseOne } from "../src/payload.mjs";
import { dashboardPage } from "../src/pages.mjs";
import { ev, sleep, startCollector } from "./harness.mjs";

const PROMPT = "the secret user text";
const KEY = "sk-live-secret";

test("a page view and a generic custom event are not agent failures", () => {
  const fields = { agent: "relay", model: "grok-4", error: "TimeoutError" };
  assert.equal(agentFailureRecord({ name: null, data: fields }), null);
  assert.equal(agentFailureRecord({ name: "signup", data: fields }), null);
  assert.equal(agentFailureRecord({ name: "$agent_failure", data: { prompt: PROMPT } }), null);
});

test("an agent failure keeps the agent, the model and the error name, and drops the prompt and the key", () => {
  const rec = agentFailureRecord({
    name: "$agent_failure",
    data: { agent: "relay", model: "grok-4", error: "TimeoutError", prompt: PROMPT, api_key: KEY },
  });
  assert.deepEqual(rec, { agent: "relay", model: "grok-4", error: "TimeoutError" });
  const body = JSON.stringify(rec);
  assert.equal(body.includes(PROMPT), false);
  assert.equal(body.includes(KEY), false);
  assert.equal(body.includes("prompt"), false);
});

test("the collector refuses an agent failure that has no agent or error name, and stores none of the prompt", () => {
  const site = "ry_site_abcdefgh";
  const missing = parseOne({ type: "event", payload: { website: site, url: "/agent", name: "$agent_failure", data: { prompt: PROMPT } } });
  assert.equal(missing.error, "agent failure needs an agent and an error name");
  const ok = parseOne({
    type: "event",
    payload: { website: site, url: "/agent", name: "$agent_failure", data: { agent: "relay", model: "grok-4", error: "TimeoutError", prompt: PROMPT, api_key: KEY } },
  });
  assert.deepEqual(ok.event.data, { agent: "relay", model: "grok-4", error: "TimeoutError" });
  assert.equal(JSON.stringify(ok.event).includes(PROMPT), false);
});

test("the Faultline issue is the error name tagged with the agent and the model", () => {
  const issue = faultlineIssue(
    { agent: "relay", model: "grok-4", error: "TimeoutError" },
    { eventId: "a".repeat(32), timestamp: "2026-10-22T00:00:00.000Z" },
  );
  assert.equal(issue.exception.values[0].type, "TimeoutError");
  assert.equal(issue.tags.agent, "relay");
  assert.equal(issue.tags.model, "grok-4");
  assert.deepEqual(issue.fingerprint, ["agent_failure", "relay", "TimeoutError"]);
  const body = JSON.stringify(issue);
  assert.equal(body.includes(PROMPT), false);
  assert.equal(body.includes("api_key"), false);
});

test("the dashboard lists agent failures apart from custom events", () => {
  const html = dashboardPage({
    token: "tok",
    origin: "https://insights.ereboslab.com",
    key: "ry_site_abcdefgh",
    site: { label: "relay.example", events_limit: 1000, paid: false },
    stats: {
      visitors: 1,
      pageviews: 1,
      engaged: 0,
      converted: 1,
      top_pages: [],
      top_referrers: [],
      utm_sources: [],
      events: [{ name: "signup", n: 1 }],
      agent_failures: [{ ts: "2026-10-22T00:00:00.000Z", agent: "relay", model: "grok-4", error: "TimeoutError" }],
    },
    speed: null,
    days: 7,
    usage: { month: 1, all: 1 },
    fresh: null,
    capLine: null,
  });
  assert.match(html, /Agent failures/);
  assert.match(html, />relay</);
  assert.match(html, />TimeoutError</);
  assert.match(html, /Custom events/);
  assert.equal(html.includes(PROMPT), false);
});

test("a stored agent failure shows on the live dashboard and on Faultline, and is not a custom event", async () => {
  const site = "ry_insights_landing";
  const sites = { [site]: { org_id: "org-insights", app_id: "insights-landing", label: "insights.ereboslab.com", events_limit: 100000 } };
  const c = await startCollector({
    d1: true,
    sites,
    bindings: {
      INGEST_URL: "",
      RAGO_URL: "",
      LANDING_SITE_KEY: site,
      STATS_TOKEN: "stats-token-123",
      FAULTLINE_DSN: "https://testkey@errors.example/9",
    },
  });
  try {
    assert.equal((await c.send(ev(site, "/"))).status, 202);
    assert.equal((await c.send(ev(site, "/pricing", { name: "signup" }))).status, 202);
    assert.equal(
      (
        await c.send(
          ev(site, "/agent", { name: "$agent_failure", data: { agent: "relay", model: "grok-4", error: "TimeoutError", prompt: PROMPT, api_key: KEY } }),
          { "user-agent": "AgentTest/1.0" },
        )
      ).status,
      202,
    );
    await sleep(3500);
    const db = await c.mf.getD1Database("DB");
    const stored = (await db.prepare("SELECT name, data FROM events WHERE name = ?").bind("$agent_failure").all()).results;
    assert.equal(stored.length, 1);
    assert.equal(JSON.stringify(stored[0]).includes(PROMPT), false);
    assert.equal(JSON.stringify(stored[0]).includes(KEY), false);
    const stats = await (await fetch(`${c.base}/api/stats?days=1`, { headers: { authorization: "Bearer stats-token-123" } })).json();
    const cur = stats.rows[0].cur;
    assert.equal(cur.pageviews, 1);
    assert.equal(cur.converted, 1);
    assert.deepEqual(cur.events, [{ name: "signup", n: 1 }]);
    assert.equal(cur.agent_failures.length, 1);
    assert.equal(cur.agent_failures[0].agent, "relay");
    assert.equal(cur.agent_failures[0].error, "TimeoutError");
    const html = await (await fetch(`${c.base}/live-demo?days=30`)).text();
    assert.match(html, /Agent failures/);
    assert.match(html, />relay</);
    assert.equal(html.includes(PROMPT), false);
    const posted = c.outbound.filter((o) => o.url.includes("/api/9/store/"));
    assert.equal(posted.length, 1);
    const issue = JSON.parse(posted[0].body);
    assert.equal(issue.exception.values[0].type, "TimeoutError");
    assert.equal(issue.tags.agent, "relay");
    assert.equal(posted[0].body.includes(PROMPT), false);
    assert.equal(posted[0].body.includes(KEY), false);
  } finally {
    await c.close();
  }
});
