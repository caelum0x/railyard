// QY1.4 (A25): alerts on saved insights and weekly dashboard subscriptions.
//  - a threshold alert mails within one scheduler tick of being made, once,
//    with an unsubscribe link; an anomaly alert fires on a spike;
//  - a subscription mails the dashboard as an HTML table at its slot, then
//    waits a week;
//  - unsubscribing stops further mail to that address; a forged link fails;
//  - without a mail service the mail is logged as not configured;
//  - the Rally Send client sends the contract's fields;
//  - another project's alerts and logs are never served.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, type Row } from "../a3/helpers.ts";
import { rallyMailer, type Mail, type Mailer, type MailOutcome } from "../../src/alerts/mail.ts";
import { evaluate } from "../../src/alerts/alerts.ts";
import { headline, zScore } from "../../src/alerts/metric.ts";
import { nextSlot } from "../../src/alerts/subscriptions.ts";
import { errors, formatFindings, lintUnits } from "../../../../policy/copy-lint.mjs";

const sent: Mail[] = [];
const fakeMailer: Mailer = {
  configured: true,
  async send(m: Mail): Promise<MailOutcome> {
    sent.push(m);
    return { status: "sent", id: `msg_${sent.length}` };
  },
};

let pg: ScratchPg;
let run: Running;
let clock = new Date("2026-10-05T10:30:00Z"); // a Monday
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });
const DAY = 86_400_000;

const signups = (day: number, n: number): Row[] =>
  Array.from({ length: n }, (_, i) => ({ event: "signup", distinct_id: `d${day}_${i}`, timestamp: new Date(clock.getTime() - day * DAY - 3600_000) }));

before(async () => {
  if (skipNoPg) return;
  process.env.PA_PUBLIC_URL = "https://pa.example";
  process.env.PA_CONSOLE_URL = "https://console.example";
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, now: () => clock, mailer: fakeMailer });
  // Ten signups a day for two weeks, then a spike of 60 yesterday.
  const rows: Row[] = [];
  for (let d = 2; d <= 15; d += 1) rows.push(...signups(d, 10));
  rows.push(...signups(1, 60));
  await insertEvents(pg, site.project_id, rows);
});

const call = (path: string, init: Parameters<typeof api>[3] = {}, s = site) => api(run, s, path, { now: clock, ...init });

async function savedTrend(name: string, s = site) {
  const r = await call("/insights/saved", { body: { name, query: { kind: "trends", interval: "day", date_range: { date_from: "-14d" }, series: [{ event: "signup" }] } } }, s);
  assert.equal(r.status, 201, JSON.stringify(r.body));
  return r.body.insight.id as string;
}

test("metric and rules: the last complete interval is watched; thresholds and anomalies decide as written", () => {
  const h = headline({ kind: "trends", interval: "day", labels: ["a", "b", "c", "d"], series: [{ label: "signup", series_index: 0, breakdown: null, data: [1, 2, 3, 4], total: 10 }] });
  assert.deepEqual([h.value, h.history], [3, [1, 2]]);
  assert.equal(evaluate({ kind: "threshold", direction: "above", threshold: 2, sensitivity: null }, 3, []).firing, true);
  assert.equal(evaluate({ kind: "threshold", direction: "below", threshold: 2, sensitivity: null }, 3, []).firing, false);
  assert.equal(zScore(10, [10, 10, 10]), null, "fewer than 7 points");
  assert.ok((zScore(60, [9, 10, 11, 10, 9, 11, 10, 10]) ?? 0) > 3);
  assert.equal(evaluate({ kind: "anomaly", direction: "either", threshold: null, sensitivity: 3 }, 60, [9, 10, 11, 10, 9, 11, 10, 10]).firing, true);
  assert.equal(evaluate({ kind: "anomaly", direction: "below", threshold: null, sensitivity: 3 }, 60, [9, 10, 11, 10, 9, 11, 10, 10]).firing, false);
  assert.equal(nextSlot(new Date("2026-10-05T10:30:00Z"), 1, 9).toISOString(), "2026-10-12T09:00:00.000Z");
  assert.equal(nextSlot(new Date("2026-10-05T10:30:00Z"), 1, 11).toISOString(), "2026-10-05T11:00:00.000Z");
});

test("a threshold alert mails within one tick, once, with an unsubscribe link; it stays quiet while firing", { skip: skipNoPg }, async () => {
  const insight = await savedTrend("Signups");
  const before = sent.length;
  const made = await call("/alerts", { body: { name: "Signup spike", insight_id: insight, kind: "threshold", direction: "above", threshold: 30, recipients: ["Ops@Shop.example", "ceo@shop.example"] } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  await run.svc.scheduler.tick(clock);
  const mails = sent.slice(before);
  assert.deepEqual(mails.map((m) => m.to).sort(), ["ceo@shop.example", "ops@shop.example"]);
  assert.equal(mails[0].subject, "Alert: Signup spike");
  assert.match(mails[0].text, /is at 60: 60 is above 30/);
  assert.match(mails[0].html, /https:\/\/pa\.example\/v1\/unsubscribe\?p=/);
  assert.ok(mails[0].unsubscribeUrl.startsWith("https://pa.example/v1/unsubscribe?"));
  assert.match(mails[0].html, /https:\/\/console\.example\/product-analytics\/insights\//);
  const alert = (await call("/alerts")).body.alerts.find((a: { name: string }) => a.name === "Signup spike");
  assert.equal(alert.state, "firing");
  assert.equal(alert.last_value, 60);
  assert.equal((await call(`/alerts/${alert.id}/log`)).body.log.filter((l: { status: string }) => l.status === "sent").length, 2);
  // An hour later it is still firing: no second mail.
  clock = new Date(clock.getTime() + 3600_000 + 1000);
  await run.svc.scheduler.tick(clock);
  assert.equal(sent.length, before + 2);
  // Every mail passes the copy lint (outbound) and says how to unsubscribe.
  for (const m of mails) {
    assert.match(m.text, /Unsubscribe from these emails: https:\/\//);
    const found = errors(lintUnits([m.subject, ...m.text.split("\n")].map((text) => ({ text, surface: "outbound", where: "alert mail" }))));
    assert.deepEqual(found, [], formatFindings(found));
  }
});

test("an anomaly alert fires on the spike", { skip: skipNoPg }, async () => {
  const insight = await savedTrend("Signups for anomaly");
  const before = sent.length;
  assert.equal((await call("/alerts", { body: { name: "Unusual signups", insight_id: insight, kind: "anomaly", sensitivity: 3, recipients: ["ops@shop.example"] } })).status, 201);
  await run.svc.scheduler.tick(new Date(clock.getTime() + 61_000));
  clock = new Date(clock.getTime() + 61_000);
  const m = sent.slice(before);
  assert.equal(m.length, 1);
  assert.match(m[0].text, /standard deviations from the earlier/);
  // Anomaly alerts are for series over time.
  const funnel = await call("/insights/saved", { body: { name: "Funnel", query: { kind: "funnels", date_range: { date_from: "-14d" }, series: [{ event: "signup" }, { event: "signup" }] } } });
  if (funnel.status === 201) assert.equal((await call("/alerts", { body: { name: "x", insight_id: funnel.body.insight.id, kind: "anomaly", recipients: ["a@b.example"] } })).status, 400);
});

test("a weekly subscription mails the dashboard as an HTML table at its slot, then waits a week", { skip: skipNoPg }, async () => {
  const insight = await savedTrend("Signups weekly");
  const dash = await call("/insights/dashboards", { body: { name: "Growth" } });
  assert.equal(dash.status, 201, JSON.stringify(dash.body));
  const dashId = dash.body.dashboard.id;
  assert.ok([200, 201].includes((await call(`/insights/dashboards/${dashId}/tiles`, { body: { insight_id: insight } })).status));
  const hour = (clock.getUTCHours() + 1) % 24;
  const made = await call("/subscriptions", { body: { dashboard_id: dashId, weekday: clock.getUTCDay(), hour_utc: hour, recipients: ["team@shop.example"] } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  const slot = new Date(made.body.subscription.next_send_at);
  const before = sent.length;
  await run.svc.scheduler.tick(new Date(slot.getTime() - 60_000));
  assert.equal(sent.length, before, "not before its slot");
  clock = new Date(slot.getTime() + 5_000);
  await run.svc.scheduler.tick(clock);
  const m = sent.slice(before);
  assert.equal(m.length, 1);
  assert.equal(m[0].subject, "Weekly: Growth");
  assert.match(m[0].html, /<table[\s\S]*Signups weekly[\s\S]*<\/table>/);
  assert.doesNotMatch(m[0].html, /<img/);
  assert.match(m[0].html, /Unsubscribe from these emails/);
  const sub = (await call("/subscriptions")).body.subscriptions[0];
  assert.equal(new Date(sub.next_send_at).getTime(), slot.getTime() + 7 * DAY);
  clock = new Date(clock.getTime() + 61_000);
  await run.svc.scheduler.tick(clock);
  assert.equal(sent.length, before + 1, "one mail per week");
});

test("unsubscribe: the link stops further mail to that address; a forged link is refused", { skip: skipNoPg }, async () => {
  const first = sent.find((m) => m.to === "ceo@shop.example")!;
  const forged = first.unsubscribeUrl.replace(/t=[^&]+/, "t=forged").replace("https://pa.example", run.base);
  assert.equal((await fetch(forged)).status, 400);
  const res = await fetch(first.unsubscribeUrl.replace("https://pa.example", run.base), { method: "POST" });
  assert.equal(res.status, 200);
  assert.match(await res.text(), /will get no more/);
  const insight = await savedTrend("Signups again");
  const before = sent.length;
  await call("/alerts", { body: { name: "Second", insight_id: insight, kind: "threshold", direction: "above", threshold: 5, recipients: ["ceo@shop.example", "ops@shop.example"] } });
  clock = new Date(clock.getTime() + 61_000);
  await run.svc.scheduler.tick(clock);
  assert.deepEqual(sent.slice(before).map((m) => m.to), ["ops@shop.example"]);
  const alert = (await call("/alerts")).body.alerts.find((a: { name: string }) => a.name === "Second");
  const statuses = (await call(`/alerts/${alert.id}/log`)).body.log.map((l: { recipient: string; status: string }) => `${l.recipient}:${l.status}`).sort();
  assert.deepEqual(statuses, ["ceo@shop.example:unsubscribed", "ops@shop.example:sent"]);
});

test("isolation: another project's token reads none of this project's alerts, subscriptions or logs", { skip: skipNoPg }, async () => {
  const mine = (await call("/alerts")).body.alerts;
  assert.ok(mine.length >= 2);
  assert.deepEqual((await call("/alerts", {}, other)).body.alerts, []);
  assert.deepEqual((await call("/subscriptions", {}, other)).body.subscriptions, []);
  assert.equal((await call(`/alerts/${mine[0].id}`, { method: "PUT", body: { enabled: false } }, other)).status, 404);
  assert.deepEqual((await call(`/alerts/${mine[0].id}/log`, {}, other)).body.log, []);
  assert.equal((await call("/alerts", { body: { name: "Steal", insight_id: mine[0].insight_id, kind: "threshold", direction: "above", threshold: 1, recipients: ["x@y.example"] } }, other)).status, 404, "another project's insight is not found");
  assert.equal((await call("/alerts", { body: { name: "Bad", insight_id: mine[0].insight_id, kind: "threshold", direction: "above", threshold: 1, recipients: ["not an email"] } })).status, 400);
});

test("without a mail service the mail is logged as not configured, never lost silently", { skip: skipNoPg }, async () => {
  const quiet = makeSite({ name: "quiet" });
  run.sites.sites.push(quiet);
  const svcDeps = run.svc.deps as { mailer: Mailer };
  const real = svcDeps.mailer;
  svcDeps.mailer = { configured: false, send: async () => ({ status: "not_configured" }) };
  try {
    await insertEvents(pg, quiet.project_id, [...signups(1, 5), ...signups(2, 5)]);
    const insight = await savedTrend("Quiet signups", quiet);
    await call("/alerts", { body: { name: "Quiet", insight_id: insight, kind: "threshold", direction: "above", threshold: 1, recipients: ["q@shop.example"] } }, quiet);
    clock = new Date(clock.getTime() + 61_000);
    await run.svc.scheduler.tick(clock);
    const a = (await call("/alerts", {}, quiet)).body.alerts[0];
    assert.equal(a.state, "firing");
    assert.deepEqual((await call(`/alerts/${a.id}/log`, {}, quiet)).body.log.map((l: { status: string }) => l.status), ["not_configured"]);
    assert.equal((await call("/alerts", {}, quiet)).body.mail_configured, false);
  } finally {
    svcDeps.mailer = real;
  }
});

test("the Rally Send client sends the transactional contract's fields and reads its answers", async () => {
  const calls: { url: string; auth: string | null; body: Record<string, unknown> }[] = [];
  const answers = [new Response(JSON.stringify({ id: "rs_1" }), { status: 202 }), new Response("{}", { status: 422 }), new Response("{}", { status: 429 })];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ url, auth: new Headers(init.headers).get("authorization"), body: JSON.parse(String(init.body)) });
    return answers.shift()!;
  }) as typeof fetch;
  const m = rallyMailer({ url: "https://rally.example/api/rally/v1/transactional/send", key: "rk_test", fetchImpl });
  const mail = { to: "a@b.example", project: "p1", subject: "S", html: "<p>h</p>", text: "t", idempotencyKey: "k1", unsubscribeUrl: "https://pa.example/v1/unsubscribe?x" };
  assert.deepEqual(await m.send(mail), { status: "sent", id: "rs_1" });
  assert.equal((await m.send(mail)).status, "failed");
  assert.match(((await m.send(mail)) as { error: string }).error, /cap/);
  assert.equal(calls[0].auth, "Bearer rk_test");
  assert.deepEqual(Object.keys(calls[0].body).sort(), ["from_local", "html", "idempotency_key", "product", "project", "subject", "tags", "text", "to", "unsubscribe_url"]);
  assert.equal(calls[0].body.product, "railyard");
});

test("every mail sent in this file carries an unsubscribe link and passes the copy lint (outbound)", { skip: skipNoPg }, () => {
  assert.ok(sent.length >= 5);
  for (const m of sent) {
    assert.match(m.text, /Unsubscribe from these emails: https:\/\//, m.subject);
    assert.match(m.html, /Unsubscribe from these emails<\/a>/, m.subject);
    const units = [m.subject, ...m.text.split("\n"), ...m.html.replace(/<[^>]+>/g, "\n").split("\n")].filter((t) => t.trim()).map((text) => ({ text, surface: "outbound", where: m.subject }));
    const found = errors(lintUnits(units));
    assert.deepEqual(found, [], formatFindings(found));
  }
});

test("console forms map to what the service takes", async () => {
  const { alertFromForm, subscriptionFromForm, destinationFromForm, destinationTarget } = await import("../../../console/app/(app)/product-analytics/_lib/a3-client.ts");
  assert.deepEqual(alertFromForm({ name: "Drop", insight_id: "i1", kind: "threshold", direction: "below", threshold: "5", recipients: "a@b.example\nc@d.example", check_every: "day" }), {
    name: "Drop", insight_id: "i1", recipients: ["a@b.example", "c@d.example"], check_every: "day", kind: "threshold", direction: "below", threshold: 5,
  });
  assert.deepEqual(alertFromForm({ name: "Odd", insight_id: "i1", kind: "anomaly", sensitivity: "2.5", recipients: "a@b.example" }), { name: "Odd", insight_id: "i1", recipients: ["a@b.example"], check_every: "hour", kind: "anomaly", direction: "either", sensitivity: 2.5 });
  assert.deepEqual(subscriptionFromForm({ dashboard_id: "d1", weekday: "3", hour_utc: "9", recipients: "a@b.example, c@d.example" }), { dashboard_id: "d1", weekday: 3, hour_utc: 9, recipients: ["a@b.example", "c@d.example"] });
  assert.deepEqual(destinationFromForm({ name: "Team", kind: "slack", target: "https://hooks.slack.com/services/x", label: "#growth" }), { name: "Team", start: "now", kind: "slack", config: { label: "#growth" }, secret: "https://hooks.slack.com/services/x" });
  assert.equal(destinationTarget({ kind: "hubspot", config: { label: "Sales" } }), "HubSpot contacts (Sales)");
  // A22: warehouse destinations from the form; the password and the key go only in `secret`.
  assert.deepEqual(destinationFromForm({ name: "Lake", kind: "clickhouse", wh_url: "https://ch.example:8443", wh_user: "writer", wh_database: "analytics", wh_secret: "pw" }), {
    name: "Lake",
    start: "now",
    kind: "clickhouse",
    config: { url: "https://ch.example:8443", database: "analytics", table: "railyard_events", user: "writer", label: "" },
    secret: "pw",
  });
  assert.deepEqual(destinationFromForm({ name: "BQ", kind: "bigquery", wh_project: "shop-analytics", wh_database: "events", wh_table: "raw", wh_secret: "{}", start: "all" }), {
    name: "BQ",
    start: "all",
    kind: "bigquery",
    config: { project_id: "shop-analytics", dataset: "events", table: "raw", label: "" },
    secret: "{}",
  });
  assert.equal(destinationTarget({ kind: "bigquery", config: { project_id: "p", dataset: "d", table: "t" } }), "BigQuery table p.d.t");
  // A23: the transform field is parsed as JSON (and passed through when it is not JSON).
  assert.deepEqual(destinationFromForm({ name: "T", kind: "webhook", url: "https://h.example/x", transform: '{"drop":["email"]}' }).transform, { drop: ["email"] });
  assert.equal(destinationFromForm({ name: "T", kind: "webhook", url: "https://h.example/x", transform: "not json" }).transform, "not json");
  assert.equal("transform" in destinationFromForm({ name: "T", kind: "webhook", url: "https://h.example/x", transform: "  " }), false);
});
