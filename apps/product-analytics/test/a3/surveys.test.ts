// Surveys: definitions are validated; the SDKs read running surveys from
// /api/surveys/ in their own shape (branding off); answers arrive as
// `survey sent` events and the results equal a hand count; targeting by page
// address (web), slot flag and cohort. A survey appears and is answered in
// Chromium with the browser SDK (web example), and through the unmodified
// React Native SDK's own survey functions under Node (Expo example).
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { registerHooks, createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { AddressInfo } from "node:net";
import { chromium } from "playwright-core";
import { surveyTargetingAnswers } from "../../src/surveys/targeting.ts";
import { targetingFlagKey } from "../../src/surveys/model.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, waitFor, type Row } from "./helpers.ts";
import { sdkEsmPath, sdkPackageDir } from "../sdk-deps.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const PKG = join(HERE, "../../../../packages/analytics-js");
const SHIM = pathToFileURL(join(HERE, "../a1/fixtures/react-native-shim.cjs")).href;
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "react-native") return { url: SHIM, shortCircuit: true, format: "commonjs" };
    return next(specifier, context);
  },
});

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });

const nps = {
  name: "Checkout feedback",
  questions: [
    { id: "q-reason", type: "single_choice", question: "What almost stopped you?", choices: ["Price", "Shipping", "Nothing"] },
    { id: "q-score", type: "rating", question: "How likely are you to recommend us?", scale: 10 },
    { id: "q-more", type: "multiple_choice", question: "What else do you use?", choices: ["Email", "Chat", "Phone"] },
    { id: "q-text", type: "open", question: "Anything else?", optional: true },
  ],
};

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET });
});

test("definitions are validated; roles, projects and the running limit hold", { skip: skipNoPg }, async () => {
  const bad = async (body: object, re: RegExp) => {
    const r = await api(run, site, "/surveys", { body });
    assert.equal(r.status, 400, JSON.stringify(r.body));
    assert.match(r.body.error, re);
  };
  await bad({ name: "x", questions: [] }, /1 to 10 questions/);
  await bad({ name: "x", questions: [{ type: "slider", question: "q" }] }, /type must be/);
  await bad({ name: "x", questions: [{ type: "rating", question: "q", scale: 4 }] }, /3, 5, 7 or 10/);
  await bad({ name: "x", questions: [{ type: "single_choice", question: "q", choices: ["a"] }] }, /2 to 20 choices/);
  await bad({ name: "x", questions: [{ type: "link", question: "q", link: "javascript:alert(1)" }] }, /https/);
  await bad({ name: "x", questions: [{ type: "open", question: "q" }], url: "(", url_match: "regex" }, /regular expression/);
  await bad({ name: "x", questions: [{ type: "open", question: "q" }], appearance: { backgroundColor: "red" } }, /color/);
  await bad({ name: "x", questions: [{ type: "open", question: "q" }], cohort_id: "00000000-0000-4000-8000-000000000000" }, /not in this app/);
  assert.equal((await api(run, site, "/surveys", { body: nps, role: "viewer" })).status, 403);
  const made = await api(run, site, "/surveys", { body: nps });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  assert.equal(made.body.survey.status, "draft");
  assert.equal((await api(run, other, `/surveys/${made.body.survey.id}`)).status, 404);
  const start = await api(run, site, `/surveys/${made.body.survey.id}/status`, { body: { status: "running" } });
  assert.equal(start.body.survey.status, "running");
  assert.equal((await api(run, site, `/surveys/${made.body.survey.id}`, { method: "PUT", body: { questions: [{ type: "open", question: "new" }] } })).status, 409);
  assert.equal((await api(run, site, `/surveys/${made.body.survey.id}`, { method: "PUT", body: { name: "Checkout feedback v2" } })).body.survey.name, "Checkout feedback v2");
  assert.equal((await api(run, site, `/surveys/${made.body.survey.id}`, { method: "DELETE" })).status, 409);
  const ids: string[] = [];
  for (let i = 0; i < 4; i += 1) {
    const s = await api(run, site, "/surveys", { body: { name: `extra ${i}`, questions: [{ type: "open", question: "q" }] } });
    ids.push(s.body.survey.id);
    assert.equal((await api(run, site, `/surveys/${s.body.survey.id}/status`, { body: { status: "running" } })).status, 200);
  }
  const sixth = await api(run, site, "/surveys", { body: { name: "sixth", questions: [{ type: "open", question: "q" }] } });
  assert.equal((await api(run, site, `/surveys/${sixth.body.survey.id}/status`, { body: { status: "running" } })).status, 409);
  for (const id of [...ids, made.body.survey.id]) await api(run, site, `/surveys/${id}/status`, { body: { status: "stopped" } });
  assert.equal((await api(run, site, `/surveys/${made.body.survey.id}`, { method: "DELETE" })).status, 200);
});

test("the SDKs read running surveys in their shape; the app's config turns surveys on", { skip: skipNoPg }, async () => {
  const cfg = async () => ((await (await fetch(`${run.base}/array/${site.token}/config`)).json()) as { surveys: false | { id: string }[] }).surveys;
  assert.equal(await cfg(), false);
  const s = (await api(run, site, "/surveys", { body: { ...nps, name: "Shape", url: "/checkout", flag_key: "new_checkout" } })).body.survey;
  let list = (await (await fetch(`${run.base}/api/surveys/?token=${site.token}`)).json()) as { surveys: Record<string, any>[] };
  assert.equal(list.surveys.length, 0, "drafts are not sent");
  await api(run, site, `/surveys/${s.id}/status`, { body: { status: "running" } });
  const inConfig = await cfg();
  assert.ok(Array.isArray(inConfig) && inConfig.some((x) => x.id === s.id), "the app's config carries the running surveys");
  const res = await fetch(`${run.base}/api/surveys/?token=${site.token}`, { headers: { origin: "https://shop.example" } });
  assert.equal(res.headers.get("access-control-allow-origin"), "https://shop.example");
  list = (await res.json()) as { surveys: Record<string, any>[] };
  const got = list.surveys.find((x) => x.id === s.id)!;
  assert.equal(got.appearance.whiteLabel, true);
  assert.deepEqual(got.conditions, { url: "/checkout", urlMatchType: "icontains" });
  assert.equal(got.linked_flag_key, "new_checkout");
  assert.equal(got.targeting_flag_key, null);
  assert.equal(got.end_date, null);
  assert.ok(got.start_date);
  assert.deepEqual(got.questions.map((q: { id: string; originalQuestionIndex: number }) => [q.id, q.originalQuestionIndex]), [["q-reason", 0], ["q-score", 1], ["q-more", 2], ["q-text", 3]]);
  assert.equal(((await (await fetch(`${run.base}/api/surveys/?token=${other.token}`)).json()) as { surveys: unknown[] }).surveys.length, 0);
  assert.equal((await fetch(`${run.base}/api/surveys/?token=site_nope000000000`)).status, 401);
  await api(run, site, `/surveys/${s.id}/status`, { body: { status: "stopped" } });
});

test("results equal a hand count: one answer per submission, keys by question id with the older index keys too", { skip: skipNoPg }, async () => {
  const s = (await api(run, site, "/surveys", { body: { ...nps, name: "Counted" } })).body.survey;
  const t = (m: number) => new Date(Date.now() - 3600_000 + m * 1000);
  const sent = (who: string, sub: string | null, props: Record<string, unknown>, m: number): Row => ({
    event: "survey sent",
    distinct_id: who,
    timestamp: t(m),
    properties: { $survey_id: s.id, $survey_name: s.name, ...(sub ? { $survey_submission_id: sub } : {}), ...props },
  });
  const rows: Row[] = [
    // A partial answer, then the full one of the same submission: counted once, the later one wins.
    sent("a", "sub-a", { "$survey_response_q-reason": "Shipping" }, 1),
    sent("a", "sub-a", { "$survey_response_q-reason": "Price", "$survey_response_q-score": 9, "$survey_response_q-more": ["Email", "Chat"], "$survey_response_q-text": "Faster delivery" }, 2),
    sent("b", "sub-b", { "$survey_response_q-reason": "Price", "$survey_response_q-score": "10", "$survey_response_q-more": ["Chat"] }, 3),
    sent("c", "sub-c", { "$survey_response_q-reason": "Nothing", "$survey_response_q-score": 9 }, 4),
    // An older SDK: index keys, no submission id.
    sent("d", null, { $survey_response: "Price", $survey_response_1: 3, $survey_response_2: ["Phone"] }, 5),
    { event: "survey shown", distinct_id: "a", timestamp: t(0), properties: { $survey_id: s.id } },
    { event: "survey shown", distinct_id: "e", timestamp: t(0), properties: { $survey_id: s.id } },
    { event: "survey dismissed", distinct_id: "e", timestamp: t(1), properties: { $survey_id: s.id } },
    sent("f", "sub-f", { "$survey_response_q-reason": "Price" }, 6),
  ];
  rows[rows.length - 1].properties!.$survey_id = "00000000-0000-4000-8000-00000000abcd";
  await insertEvents(pg, site.project_id, rows);
  await insertEvents(pg, other.project_id, [sent("z", "sub-z", { "$survey_response_q-reason": "Price" }, 1)]);
  const r = (await api(run, site, `/surveys/${s.id}/results`)).body.results;
  assert.equal(r.responses, 4);
  assert.equal(r.shown, 2);
  assert.equal(r.dismissed, 1);
  const byId = Object.fromEntries(r.questions.map((q: { id: string }) => [q.id, q]));
  assert.deepEqual(byId["q-reason"].counts, { Price: 3, Nothing: 1 });
  assert.deepEqual(byId["q-score"].counts, { "9": 2, "10": 1, "3": 1 });
  assert.deepEqual(byId["q-more"].counts, { Email: 1, Chat: 2, Phone: 1 });
  assert.deepEqual(byId["q-text"].texts, ["Faster delivery"]);
  assert.equal(byId["q-text"].answered, 1);
});

test("cohort targeting: the survey's targeting flag is on exactly for the cohort's members; a response limit stops it showing", { skip: skipNoPg }, async () => {
  await fetch(`${run.base}/batch/`, { method: "POST", body: JSON.stringify({ api_key: site.token, batch: [
    { event: "$identify", distinct_id: "member@shop.example", properties: { $anon_distinct_id: "anon-member" }, timestamp: new Date().toISOString() },
    { event: "$identify", distinct_id: "outsider@shop.example", properties: {}, timestamp: new Date().toISOString() },
  ] }) });
  await run.svc.deps.writer.flush();
  const seg = await api(run, site, "/insights/segments", { body: { name: "Paying", kind: "static" } });
  assert.equal(seg.status, 201, JSON.stringify(seg.body));
  const up = await fetch(`${run.base}/v1/projects/${site.project_id}/insights/segments/${seg.body.segment.id}/csv`, { method: "POST", headers: { authorization: `Bearer ${await (await import("./helpers.ts")).token(site)}`, "content-type": "text/csv" }, body: "distinct_id\nmember@shop.example\n" });
  assert.equal(up.status, 200, await up.text());
  const s = (await api(run, site, "/surveys", { body: { name: "Members only", questions: [{ type: "open", question: "Why did you upgrade?" }], cohort_id: seg.body.segment.id, response_limit: 1 } })).body.survey;
  await api(run, site, `/surveys/${s.id}/status`, { body: { status: "running" } });
  const listed = ((await (await fetch(`${run.base}/api/surveys/?token=${site.token}`)).json()) as { surveys: { id: string; targeting_flag_key: string }[] }).surveys.find((x) => x.id === s.id)!;
  assert.equal(listed.targeting_flag_key, targetingFlagKey(s.id));
  const on = async (id: string) => (await surveyTargetingAnswers(run.svc.deps.db, site.project_id, id)).find((a) => a.key === targetingFlagKey(s.id))?.enabled;
  assert.equal(await on("member@shop.example"), true);
  assert.equal(await on("anon-member"), true, "any id of the member");
  assert.equal(await on("outsider@shop.example"), false);
  assert.equal(await on("never-seen"), false);
  await insertEvents(pg, site.project_id, [{ event: "survey sent", distinct_id: "member@shop.example", timestamp: new Date(), properties: { $survey_id: s.id, $survey_submission_id: "one", "$survey_response_0": "More seats" } }]);
  const after = ((await (await fetch(`${run.base}/api/surveys/?token=${site.token}`)).json()) as { surveys: { id: string }[] }).surveys;
  assert.equal(after.some((x) => x.id === s.id), false, "limit reached: no longer sent");
  await api(run, site, `/surveys/${s.id}/status`, { body: { status: "stopped" } });
});

test("web example: the survey shows on the targeted page in Chromium and its answer lands in the results", { skip: skipNoPg || (existsSync(chromium.executablePath()) ? false : "no cached Chromium"), timeout: 120_000 }, async () => {
  const s = (await api(run, site, "/surveys", { body: { name: "Web checkout", url: "/checkout", questions: [{ id: "q1", type: "single_choice", question: "What almost stopped you?", choices: ["Price", "Shipping"], buttonText: "Send" }] } })).body.survey;
  const elsewhere = (await api(run, site, "/surveys", { body: { name: "Web pricing", url: "/pricing", questions: [{ type: "open", question: "Pricing question" }] } })).body.survey;
  for (const x of [s, elsewhere]) await api(run, site, `/surveys/${x.id}/status`, { body: { status: "running" } });
  const page = `<!doctype html><html><head><title>Checkout</title><script type="importmap">{"imports":{"posthog-js":"/vendor/sdk.mjs"}}</script></head>
<body><h1>Checkout</h1><script type="module">
  import { createAnalytics } from "/pkg/index.mjs";
  window.analytics = createAnalytics({ site: ${JSON.stringify(site.token)}, host: ${JSON.stringify(run.base)}, requireConsent: false });
</script></body></html>`;
  const files: Record<string, [string, string]> = {
    "/checkout": [page, "text/html"],
    "/vendor/sdk.mjs": [readFileSync(sdkEsmPath(PKG), "utf8"), "text/javascript"],
    "/pkg/index.mjs": [readFileSync(join(PKG, "src/index.mjs"), "utf8"), "text/javascript"],
    "/pkg/env.mjs": [readFileSync(join(PKG, "src/env.mjs"), "utf8"), "text/javascript"],
  };
  const srv = createServer((req, res) => {
    const f = files[(req.url ?? "/").split("?")[0]];
    if (!f) return void res.writeHead(404).end();
    res.writeHead(200, { "content-type": f[1] }).end(f[0]);
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  const browser = await chromium.launch({ headless: true, channel: "chromium", args: ["--disable-blink-features=AutomationControlled"] });
  try {
    const p = await browser.newPage({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36" });
    const seen: string[] = [];
    p.on("request", (r) => seen.push(r.url().replace(run.base, "")));
    p.on("console", (m) => seen.push(`console ${m.type()}: ${m.text()}`));
    p.on("pageerror", (e) => seen.push(`pageerror: ${e.message}`));
    p.on("response", (r) => { if (r.status() >= 400) seen.push(`HTTP ${r.status()} ${r.url()}`); });
    await p.goto(`http://localhost:${(srv.address() as AddressInfo).port}/checkout`);
    try {
      await p.getByText("What almost stopped you?").waitFor({ timeout: 20_000 });
    } catch (err) {
      const diag = await p.evaluate(async () => {
        const c = (window as any).analytics.client;
        const out: any = { capturing: c.is_capturing?.(), focus: c.surveys?._surveyManager?._surveyInFocus, rendered: c.surveys?._surveyManager?._surveyIsRendered, cfgSurveys: c.surveys?._isSurveysEnabled };
        await new Promise<void>((r) => c.getActiveMatchingSurveys((s: any[]) => ((out.active = s.map((x) => x.id)), r()), true));
        await new Promise<void>((r) => c.getSurveys((s: any[]) => ((out.all = s.map((x: any) => [x.id, x.name, c.surveys?._surveyManager?.checkSurveyEligibility?.(x)])), r()), true));
        out.mgr = !!c.surveys?._surveyManager;
        const host = [...document.querySelectorAll("div")].find((d) => d.shadowRoot);
        out.host = host ? host.className : null;
        out.shadow = host?.shadowRoot?.innerHTML.slice(0, 1500) ?? null;
        out.rect = host?.shadowRoot?.querySelector(".ph-survey")?.getBoundingClientRect?.() ?? null;
        return out;
      });
      console.error(seen.join("\n"), JSON.stringify(diag, null, 1));
      throw err;
    }
    assert.equal(await p.getByText("Pricing question").count(), 0, "the survey for another page does not show");
    assert.equal(await p.getByText(/survey by/i).count(), 0, "no branding");
    await p.getByText("Shipping", { exact: true }).click({ timeout: 5000 });
    // The SDK labels its submit button for screen readers; the visible text is the survey's own.
    const submit = p.getByRole("button", { name: "Submit survey" });
    assert.equal(await submit.textContent(), "Send");
    await submit.click({ timeout: 5000 });
    const ok = await waitFor(async () => {
      await run.svc.deps.writer.flush();
      return (await api(run, site, `/surveys/${s.id}/results`)).body.results.responses === 1;
    }, 20_000, 300);
    assert.ok(ok, "the answer was stored");
    const r = (await api(run, site, `/surveys/${s.id}/results`)).body.results;
    assert.deepEqual(r.questions[0].counts, { Shipping: 1 });
    assert.equal(r.shown, 1);
  } finally {
    await browser.close();
    srv.close();
    for (const x of [s, elsewhere]) await api(run, site, `/surveys/${x.id}/status`, { body: { status: "stopped" } });
  }
});

test("Expo example: the React Native SDK reads the survey, matches it on the device and sends the answer with its own function", { skip: skipNoPg }, async () => {
  const s = (await api(run, site, "/surveys", { body: { name: "In-app", questions: [{ id: "rn1", type: "rating", question: "How easy was it?", scale: 5 }] } })).body.survey;
  const web = (await api(run, site, "/surveys", { body: { name: "Web only", url: "/checkout", questions: [{ type: "open", question: "q" }] } })).body.survey;
  for (const x of [s, web]) await api(run, site, `/surveys/${x.id}/status`, { body: { status: "running" } });
  const { createNativeAnalytics } = await import("../../../../packages/analytics-js/src/react-native.mjs");
  const disk = new Map<string, string>();
  const app = createNativeAnalytics({ site: site.token, host: run.base, consent: "granted", sdkOptions: { customStorage: { getItem: (k: string) => disk.get(k) ?? null, setItem: (k: string, v: string) => void disk.set(k, v) }, flushAt: 1, flushInterval: 0, preloadFeatureFlags: false, captureAppLifecycleEvents: false } });
  const client = app.client as { getSurveys(): Promise<Record<string, unknown>[]>; flush(): Promise<void>; identify(id: string): void };
  client.identify("rn-user-1");
  let surveys: Record<string, unknown>[] = [];
  assert.ok(await waitFor(async () => (surveys = await client.getSurveys()).length === 2, 15_000, 200), "the SDK read the surveys from the app's config");
  assert.deepEqual(surveys.map((x) => x.id).sort(), [s.id, web.id].sort());
  const rnRequire = createRequire(join(sdkPackageDir(PKG, "posthog-react-native"), "package.json"));
  const { getActiveMatchingSurveys } = rnRequire("./dist/surveys/getActiveMatchingSurveys.js");
  const active = getActiveMatchingSurveys(surveys, {}, [], new Set());
  assert.deepEqual(active.map((x: { id: string }) => x.id), [s.id], "the page-address survey is web only");
  const { sendSurveyEvent, sendSurveyShownEvent } = rnRequire("./dist/surveys/components/Surveys.js");
  sendSurveyShownEvent(active[0], client);
  // The SDK UI keys answers by the response key of the question id.
  sendSurveyEvent({ "$survey_response_rn1": 4 }, active[0], client);
  const ok = await waitFor(async () => {
    await client.flush().catch(() => undefined);
    await run.svc.deps.writer.flush();
    return (await api(run, site, `/surveys/${s.id}/results`)).body.results.responses === 1;
  }, 20_000, 300);
  assert.ok(ok, "the answer was stored");
  const r = (await api(run, site, `/surveys/${s.id}/results`)).body.results;
  assert.deepEqual(r.questions[0].counts, { "4": 1 });
  assert.equal(r.shown, 1);
});
