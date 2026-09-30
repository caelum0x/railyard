// The console side: its org token (WebCrypto, no new dependency) is accepted
// by the service and scoped; its client reads the live status; the screens,
// READMEs and type files pass the copy denylist (app-ui and docs) and name no
// upstream product; exactly one nav line was added.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import { verifyConsoleToken } from "../../src/tenancy/console-token.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const RY = join(HERE, "../../../..");
const PA_UI = join(RY, "apps/console/app/(app)/product-analytics");
const SECRET = "k".repeat(48);

const { signOrgToken, TOKEN_TTL_SECONDS } = await import(join(PA_UI, "_lib/token.ts"));
const { paFetch, paConfig, pickApp } = await import(join(PA_UI, "_lib/http.ts"));
const lint = await import(join(RY, "policy/copy-lint.mjs"));

const walk = (dir: string): string[] => readdirSync(dir).flatMap((f) => (statSync(join(dir, f)).isDirectory() ? walk(join(dir, f)) : [join(dir, f)]));

test("the console's token is accepted by the service, expires in 5 minutes, and is tamper-proof", async () => {
  const org = randomUUID();
  const app = randomUUID();
  const now = new Date("2026-09-30T12:00:00Z");
  const tok = await signOrgToken(SECRET, { org_id: org, app_ids: [app], role: "owner" }, now);
  assert.equal(TOKEN_TTL_SECONDS, 300);
  assert.deepEqual(await verifyConsoleToken(SECRET, tok, now), { org_id: org, app_ids: [app], role: "owner", exp: Math.floor(now.getTime() / 1000) + 300 });
  assert.equal(await verifyConsoleToken(SECRET, tok, new Date(now.getTime() + 301_000)), null);
  assert.equal(await verifyConsoleToken("x".repeat(48), tok, now), null);
  const [h, , sig] = tok.split(".");
  const forged = `${h}.${Buffer.from(JSON.stringify({ org_id: org, app_ids: [app, randomUUID()], role: "owner", iat: 1790769600, exp: 1790769900 })).toString("base64url")}.${sig}`;
  assert.equal(await verifyConsoleToken(SECRET, forged, now), null);
});

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site], { consoleSecret: SECRET });
});

test("the console client reads the live status and first-event proof through the service", { skip: skipNoPg }, async () => {
  const cfg = paConfig({ PRODUCT_ANALYTICS_URL: `${run.base}/`, PRODUCT_ANALYTICS_CONSOLE_SECRET: SECRET });
  assert.ok(cfg);
  assert.equal(paConfig({ PRODUCT_ANALYTICS_URL: run.base }), null, "no secret: the page says the service is arriving");
  const scope = { org_id: site.org_id, app_ids: [site.project_id] };
  const before = await paFetch(cfg, scope, `/v1/projects/${site.project_id}/status`);
  assert.equal(before.first_event, null);
  await fetch(`${run.base}/e/`, { method: "POST", body: JSON.stringify({ event: "signed_up", uuid: randomUUID(), properties: { token: site.token, distinct_id: "u1" } }) });
  await run.svc.deps.writer.flush();
  const after = await paFetch(cfg, scope, `/v1/projects/${site.project_id}/status`);
  assert.equal(after.first_event.event, "signed_up");
  const feed = await paFetch(cfg, scope, `/v1/projects/${site.project_id}/events?limit=5`);
  assert.equal(feed.events[0].event, "signed_up");
  await assert.rejects(paFetch(cfg, { org_id: randomUUID(), app_ids: [site.project_id] }, `/v1/projects/${site.project_id}/status`), /not in your organization/);
  assert.equal(pickApp([{ id: "1", name: "a", site_key: "k" }, { id: "2", name: "b", site_key: "k" }], "b").id, "2");
  assert.equal(pickApp([{ id: "1", name: "a", site_key: "k" }], "zzz").id, "1");
});

test("console screens pass the copy denylist (app-ui) and name no upstream product", () => {
  const files = walk(PA_UI).filter((f) => /\.(tsx?|mjs)$/.test(f));
  assert.ok(files.length >= 8);
  const units = files.flatMap((f) => {
    const src = readFileSync(f, "utf8");
    const strings = [...src.matchAll(/>([^<>{}]+)</g), ...src.matchAll(/"([^"\n]{3,})"/g), ...src.matchAll(/`([^`\n]{3,})`/g)].map((m) => m[1]);
    const where = relative(RY, f);
    assert.doesNotMatch(src, /posthog|rrweb|umami|mixpanel|amplitude/i, where);
    return strings.map((t) => ({ text: t, surface: "app-ui", where, path: where }));
  });
  const found = lint.errors(lint.lintUnits(units));
  assert.deepEqual(found, [], lint.formatFindings(found));
});

test("READMEs and type files pass the copy denylist (docs) and name no upstream product", () => {
  const docs = ["apps/product-analytics/README.md", "packages/analytics-js/README.md", "packages/analytics-node/README.md"];
  for (const d of docs) {
    const md = readFileSync(join(RY, d), "utf8");
    assert.doesNotMatch(md.replace(/`NOTICE`/g, ""), /posthog|rrweb/i, d);
    const found = lint.errors(lint.lintUnits(lint.markdownUnits(md, { where: d })));
    assert.deepEqual(found, [], lint.formatFindings(found));
  }
  for (const t of ["packages/analytics-js/src/index.d.ts", "packages/analytics-js/src/react-native.d.ts", "packages/analytics-node/src/index.d.ts"]) {
    const src = readFileSync(join(RY, t), "utf8");
    assert.doesNotMatch(src, /posthog|rrweb/i, t);
    const units = [...src.matchAll(/\/\*\*([^*]+)\*\//g)].map((m) => ({ text: m[1], surface: "docs", where: t }));
    assert.deepEqual(lint.errors(lint.lintUnits(units)), []);
  }
});

test("the console nav has exactly one product analytics line", () => {
  // Checked on the file, not on `git diff main`: once this lane is merged,
  // main already holds the line and the diff is empty.
  const nav = readFileSync(join(RY, "apps/console/components/Nav.tsx"), "utf8");
  const lines = nav.split("\n").filter((l) => l.includes("/product-analytics"));
  assert.deepEqual(lines, ['  { href: "/product-analytics", label: "Product analytics" },']);
});
