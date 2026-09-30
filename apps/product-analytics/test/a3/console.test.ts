// The console side of A3: the client reads every recordings, click maps,
// surveys, experiments and exports endpoint through a real service; the form
// mappings are right; an experiment starts and stops only through the flags
// slot's setFlag (a fixture of the wave-p2-railyard contract: flags merge
// pending) and the service then reads the flag back as live; the screens and
// the service's public pages pass the copy denylist, name no upstream
// product, use no emoji and keep one primary action; A3 commits touch only
// A3's paths.
import { before, after, test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";
import { MemoryFlagsReader, type SlotFlag } from "../../src/flags/client.ts";
import { MemoryObjectStore, useReplayStore } from "../../src/replay/object-store.ts";
import { pageHtml } from "../../src/replay/player-pages.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET } from "./helpers.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const RY = join(HERE, "../../../..");
const SERVICE = join(RY, "apps/product-analytics");
const CONSOLE = join(RY, "apps/console");
const PA_UI = join(CONSOLE, "app/(app)/product-analytics");
const MINE = ["replay", "heatmaps", "surveys", "experiments", "exports", "_lib/a3-client.ts"].map((p) => join(PA_UI, p));

const client = await import(join(PA_UI, "_lib/a3-client.ts"));
const slotModule = await import(join(PA_UI, "experiments/_slot/flags-slot.ts"));
const lint = await import(join(RY, "policy/copy-lint.mjs"));

const walk = (p: string): string[] => (statSync(p).isDirectory() ? readdirSync(p).flatMap((f) => walk(join(p, f))) : [p]);
const myFiles = () => MINE.flatMap(walk).filter((f) => /\.(tsx?|mjs)$/.test(f));

test("form mappings: surveys, experiments and destinations become what the service accepts", () => {
  assert.deepEqual(client.surveyFromForm({ name: "NPS", question: "How likely?", qtype: "rating", scale: "10", target: "url", url: "/checkout", url_match: "exact", limit: "50" }), {
    name: "NPS",
    type: "popover",
    questions: [{ type: "rating", question: "How likely?", scale: 10 }],
    url: "/checkout",
    url_match: "exact",
    response_limit: 50,
  });
  const choice = client.surveyFromForm({ name: "Why", question: "Why?", qtype: "single_choice", choices: "Price\n\n Shipping \n", target: "segment", segment_id: "11111111-1111-4111-8111-111111111111", button: "Send" });
  assert.deepEqual(choice.questions, [{ type: "single_choice", question: "Why?", choices: ["Price", "Shipping"], buttonText: "Send" }]);
  assert.equal(choice.cohort_id, "11111111-1111-4111-8111-111111111111");
  assert.equal(client.surveyFromForm({ name: "x", question: "q", qtype: "open", target: "flag", flag_key: "beta", url: "/ignored" }).url, undefined);
  assert.deepEqual(client.parseVariants("control 34\ntest: 33\nTest_B,33\n"), [
    { key: "control", weight: 34 },
    { key: "test", weight: 33 },
    { key: "test_b", weight: 33 },
  ]);
  assert.deepEqual(client.experimentFromForm({ name: "E", flag_key: "pay", metric_kind: "sum", metric_event: "purchase", metric_property: "amount", method: "frequentist", rollout: "50" }), {
    name: "E",
    description: "",
    flag_key: "pay",
    flags_env: "production",
    variants: [{ key: "control", weight: 50 }, { key: "test", weight: 50 }],
    rollout: 50,
    metric: { kind: "sum", event: "purchase", property: "amount" },
    method: "frequentist",
  });
  assert.deepEqual(client.destinationFromForm({ name: "W", kind: "webhook", url: "https://h.example/x", events: "signup, purchase\n" }), { name: "W", start: "now", events: ["signup", "purchase"], kind: "webhook", config: { url: "https://h.example/x" } });
  assert.equal(client.destinationFromForm({ name: "S", kind: "s3", endpoint: "https://s3.example", bucket: "b", access_key_id: "k", secret: "s", start: "all" }).secret, "s");
  assert.deepEqual(client.answerKeys({ id: "q", type: "rating", question: "q", scale: 10, optional: false }, { "7": 1 }), ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
  assert.deepEqual(client.answerKeys({ id: "q", type: "single_choice", question: "q", choices: ["A", "B"], optional: false }, { B: 2, Other: 1 }), ["A", "B", "Other"]);
  assert.equal(client.duration(4_000), "4 s");
  assert.equal(client.duration(125_000), "2 min 5 s");
  assert.equal(client.duration(3_725_000), "1 h 2 min");
  assert.equal(client.percent(0.1234), "12.3%");
  assert.equal(client.percent(0.5), "50%");
});

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const KEY = "ry_flags_consoleabcdefghijklmnopqrstuv01";
const slotFlags: Record<string, Record<string, SlotFlag>> = { [KEY]: {} };

before(async () => {
  if (skipNoPg) return;
  useReplayStore(new MemoryObjectStore());
  process.env.PA_EXPORTS_KEY = "b".repeat(64);
  process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";
  pg = await migratedPg();
  run = await startService(pg, [site], { consoleSecret: SECRET, flags: new MemoryFlagsReader(slotFlags) });
});

test("the console client reads every A3 endpoint through the service", { skip: skipNoPg }, async () => {
  const a3 = client.a3Api({ url: run.base, secret: SECRET }, { org_id: site.org_id, app_ids: [site.project_id] }, site.project_id);
  assert.equal((await a3.recordingSettings()).settings.replay_enabled, false);
  assert.equal((await a3.saveRecordingSettings({ replay_enabled: true, heatmaps_enabled: true })).settings.heatmaps_enabled, true);
  assert.deepEqual((await a3.replays()).replays, []);
  assert.deepEqual((await a3.replaysForPeople(["00000000-0000-4000-8000-000000000001"])).replays, []);
  await assert.rejects(a3.replay("nope"), /does not exist/);
  await insertEvents(pg, site.project_id, [{ event: "$$heatmap", distinct_id: "u", timestamp: new Date(Date.now() - 60_000), properties: { $heatmap_data: { "https://shop.example/": [{ x: 10, y: 10, type: "click" }] }, $viewport_width: 1000 } }]);
  assert.equal((await a3.heatmapUrls()).urls[0].url, "https://shop.example/");
  assert.equal((await a3.heatmap({ url: "https://shop.example/" })).total, 1);
  assert.match((await a3.heatmapLink({ url: "https://shop.example/", width: 1280 })).url, /\/player\/heatmap\?r=heatmap%3A/);
  const survey = (await a3.createSurvey(client.surveyFromForm({ name: "S", question: "Q?", qtype: "open", target: "everyone" }))).survey;
  assert.equal((await a3.surveys()).surveys.length, 1);
  assert.equal((await a3.setSurveyStatus(survey.id, "running")).survey.status, "running");
  assert.equal((await a3.surveyResults(survey.id)).results.responses, 0);
  await a3.setSurveyStatus(survey.id, "stopped");
  assert.equal((await a3.deleteSurvey(survey.id)).deleted, true);
  const exp = (await a3.createExperiment(client.experimentFromForm({ name: "E", flag_key: "pay_button", metric_event: "purchase" }))).experiment;
  assert.equal((await a3.experiments()).experiments.length, 1);
  assert.equal((await a3.experiment(exp.id)).flag_state, "not_connected");
  assert.equal((await a3.experimentResults(exp.id)).results.variants.length, 2);
  const made = await a3.createDestination(client.destinationFromForm({ name: "W", kind: "webhook", url: "http://127.0.0.1:9/x" }));
  assert.match(made.signing_secret ?? "", /^whsec_/);
  assert.equal((await a3.destinations()).destinations.length, 1);
  assert.equal((await a3.updateDestination(made.destination.id, { enabled: false })).destination.enabled, false);
  assert.equal((await a3.testDestination(made.destination.id)).ok, false);
  assert.deepEqual((await a3.runs(made.destination.id)).runs, []);
  assert.equal((await a3.deleteDestination(made.destination.id)).deleted, true);
  const csv = await a3.savedInsightCsv("00000000-0000-4000-8000-000000000000");
  assert.equal(csv.status, 404);
  const other = client.a3Api({ url: run.base, secret: SECRET }, { org_id: site.org_id, app_ids: [site.project_id] }, "00000000-0000-4000-8000-000000000000");
  await assert.rejects(other.surveys(), /not in your organization/);
});

// The flags slot's contract (wave-p2-railyard lib/flags/service.ts setFlag), in memory: the fixture
// writes the flag where the slot's public client API (MemoryFlagsReader here) reads it.
function fixtureSlot(orgId: string) {
  const calls: { env: string; input: { key: string; enabled: boolean; value: string | null }; actor: string }[] = [];
  const env = { id: "env-1", org_id: orgId, app: "shop", env: "production", client_key: KEY };
  const slot = {
    async envForClientKey(org: string, key: string) {
      return org === env.org_id && key === env.client_key ? env : null;
    },
    async setFlag(e: typeof env, input: { key: string; enabled: boolean; value: string | null }, actor: string) {
      if (!/^[a-z][a-z0-9_]{0,63}$/.test(input.key)) throw new Error("key must start with a letter");
      if (input.value !== null && input.value.length > 2000) throw new Error("value must be text of at most 2000 characters");
      calls.push({ env: e.env, input, actor });
      slotFlags[e.client_key] = { ...slotFlags[e.client_key], [input.key]: { enabled: input.enabled, value: input.value ?? null } };
      return { key: input.key, enabled: input.enabled, value: input.value ?? null, description: null, updated_at: new Date().toISOString() };
    },
  };
  return { slot, calls };
}

test("an experiment starts and stops only through the flags slot's setFlag, and the service then reads it as live", { skip: skipNoPg }, async () => {
  const a3 = client.a3Api({ url: run.base, secret: SECRET }, { org_id: site.org_id, app_ids: [site.project_id] }, site.project_id);
  const exp = (await a3.createExperiment(client.experimentFromForm({ name: "Checkout copy", flag_key: "checkout_copy", metric_event: "purchase" }))).experiment;
  const { slot, calls } = fixtureSlot(site.org_id);
  const now = new Date();
  await assert.rejects(slotModule.changeExperiment(a3, null, site.org_id, exp.id, "start", undefined, "experiment:me", now), /not connected on this deployment/);
  assert.equal(slotModule.connectedSlot(), null, "flags merge pending: no slot on main yet");
  await assert.rejects(slotModule.changeExperiment(a3, slot, site.org_id, exp.id, "start", undefined, "experiment:me", now), /Setup screen/);
  assert.equal((await api(run, site, "/settings", { method: "PUT", body: { flags_client_key: KEY } })).status, 200);
  await assert.rejects(slotModule.changeExperiment(a3, slot, "00000000-0000-4000-8000-000000000009", exp.id, "start", undefined, "experiment:me", now), /does not belong/);
  assert.equal(calls.length, 0);
  const flag = await slotModule.changeExperiment(a3, slot, site.org_id, exp.id, "start", undefined, "experiment:me", now);
  assert.equal(calls.length, 1, "exactly one flag write, through setFlag");
  assert.equal(calls[0].env, "production");
  assert.equal(calls[0].actor, "experiment:me");
  assert.deepEqual(JSON.parse(flag.value!), { rollout: 100, variants: [{ key: "control", weight: 50 }, { key: "test", weight: 50 }], experiment: exp.id });
  const detail = await a3.experiment(exp.id);
  assert.equal(detail.experiment.status, "running");
  assert.equal(detail.flag_state, "live");
  await slotModule.changeExperiment(a3, slot, site.org_id, exp.id, "stop", "test", "experiment:me", now);
  assert.equal(calls.length, 2);
  assert.deepEqual(JSON.parse(calls[1].input.value!).variants, [{ key: "test", weight: 100 }]);
  assert.equal((await a3.experiment(exp.id)).experiment.status, "stopped");
  const other = (await a3.createExperiment(client.experimentFromForm({ name: "Staging only", flag_key: "staging_flag", flags_env: "staging", metric_event: "purchase" }))).experiment;
  await assert.rejects(slotModule.changeExperiment(a3, slot, site.org_id, other.id, "start", undefined, "experiment:me", now), /runs in the staging flags environment/);
  assert.equal(calls.length, 2);
});

// Rendering the shared parts with the console's own React (no dependency added).
const REACT_DOM = join(CONSOLE, "node_modules/react-dom/server.js");
const OUT = join(CONSOLE, ".a3-render-test");
after(() => rmSync(OUT, { recursive: true, force: true }));

test("the shared screen parts render without NaN or undefined", { skip: existsSync(REACT_DOM) ? false : "console dependencies not linked" }, async () => {
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(OUT, { recursive: true });
  const src = readFileSync(join(PA_UI, "replay/_parts/A3Shell.tsx"), "utf8")
    .replace(/import \{ Shell \} from "[^"]+";/, "const Shell = ({ children }: { children: unknown }) => children;")
    .replace(/import \{ liveModules, type LiveModules \} from "[^"]+";/, `import { liveModules } from ${JSON.stringify(pathToFileURL(join(PA_UI, "replay/_parts/live.ts")).href)};`);
  const js = ts.transpileModule(src, { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText;
  writeFileSync(join(OUT, "A3Shell.mjs"), js.replace(/from "react\/jsx-runtime"/, `from ${JSON.stringify(pathToFileURL(join(CONSOLE, "node_modules/react/jsx-runtime.js")).href)}`));
  const React = await import(pathToFileURL(join(CONSOLE, "node_modules/react/index.js")).href);
  const server = await import(pathToFileURL(REACT_DOM).href);
  const m = await import(pathToFileURL(join(OUT, "A3Shell.mjs")).href);
  const render = (server.default ?? server).renderToStaticMarkup as (el: unknown) => string;
  const h = (React.default ?? React).createElement;
  const bar = render(h(m.ShareBar, { label: "Price", value: 3, total: 4 }));
  assert.match(bar, /Price/);
  assert.match(bar, /3 \(75%\)/);
  assert.match(render(h(m.ShareBar, { label: "None", value: 0, total: 0 })), /0 \(0%\)/);
  const shell = render(h(m.A3Shell, { title: "Recordings", names: ["shop"], app: "shop", current: "/product-analytics/replay", live: { replay: true, flags: true } }, h(m.Notice, { text: m.NO_APP })));
  for (const s of m.A3_SCREENS) assert.ok(shell.includes(`${s.href}?app=shop`), s.href);
  const off = render(h(m.A3Shell, { title: "Surveys", names: ["shop"], app: "shop", current: "/product-analytics/surveys", live: { replay: false, flags: false } }, null));
  assert.doesNotMatch(off, /Experiments|Recordings|Click maps/, "switched-off modules are not named");
  assert.match(off, /Surveys/);
  assert.match(shell, /aria-current="page"/);
  assert.doesNotMatch(bar + shell, /NaN|undefined/);
});

// Copy lint: every string of these files is a unit, except URL paths and
// data-copy-source markers (code, not copy). Screens behind a module switch
// are linted with that switch on, and must return early while it is off.
const GATED: { dir: string; flag: "RAILYARD_REPLAY_LIVE" | "RAILYARD_FLAGS_LIVE"; mod: "replay" | "flags" }[] = [
  { dir: join(PA_UI, "replay"), flag: "RAILYARD_REPLAY_LIVE", mod: "replay" },
  { dir: join(PA_UI, "heatmaps"), flag: "RAILYARD_REPLAY_LIVE", mod: "replay" },
  { dir: join(PA_UI, "experiments"), flag: "RAILYARD_FLAGS_LIVE", mod: "flags" },
];
const SHELL = join(PA_UI, "replay/_parts/A3Shell.tsx");

function unitsOf(f: string) {
  const src = readFileSync(f, "utf8");
  const where = relative(RY, f);
  const attrs = new Set([...src.matchAll(/data-copy-source="([^"]*)"/g)].map((m) => m[1]));
  const strings = [...src.matchAll(/>([^<>{}]+)</g), ...src.matchAll(/"([^"\n]{3,})"/g), ...src.matchAll(/`([^`\n]{3,})`/g)].map((m) => m[1]);
  const isPath = (t: string) => /^\/[^\s]*$/.test(t.trim().replace(/\$\{[^}]*\}/g, ""));
  return strings.filter((t) => !attrs.has(t) && !isPath(t)).map((t) => ({ text: t, surface: "app-ui", where, path: where }));
}

test("screens and public pages pass the copy denylist, name no upstream product, use no emoji and keep one primary action", () => {
  const files = myFiles();
  assert.ok(files.length >= 14, `found ${files.length} files`);
  const findings: unknown[] = [];
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    const where = relative(RY, f);
    assert.doesNotMatch(src, /posthog|rrweb|umami|mixpanel|amplitude|hogql|growthbook|simpleheat|jstat/i, where);
    assert.doesNotMatch(src, /\bindie\b/i, where);
    assert.doesNotMatch(src, /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u, where);
    if (/page\.tsx$/.test(f)) assert.ok((src.match(/className="primary"/g) ?? []).length <= 1, `${where}: one primary action`);
    const gate = GATED.find((g) => f.startsWith(g.dir + "/"));
    if (gate && /page\.tsx$/.test(f)) {
      assert.match(src, new RegExp(`if \\(!liveModules\\(\\)\\.${gate.mod}\\) \\{\\n    return \\(`), `${where} returns early while ${gate.flag} is off`);
      for (const action of src.matchAll(/"use server";\n(.*)\n/g)) assert.match(action[1], new RegExp(`liveModules\\(\\)\\.${gate.mod}`), `${where}: its actions refuse while ${gate.flag} is off`);
    }
    const flags = f === SHELL ? { RAILYARD_REPLAY_LIVE: "1", RAILYARD_FLAGS_LIVE: "1" } : gate ? { [gate.flag]: "1" } : {};
    findings.push(...lint.errors(lint.lintUnits(unitsOf(f), { flags })));
  }
  // The service's own pages: the text the player and map pages show.
  for (const name of ["replay.js", "heatmap.js"]) {
    const f = join(SERVICE, "public/player", name);
    const shown = [...readFileSync(f, "utf8").matchAll(/"([^"\n]{12,})"/g)].map((m) => m[1]).filter((s) => / /.test(s));
    for (const t of shown) assert.doesNotMatch(t, /posthog|rrweb|\bindie\b/i, f);
    findings.push(...lint.errors(lint.lintUnits(shown.map((t) => ({ text: t, surface: "app-ui", where: relative(RY, f), path: relative(RY, f) })))));
  }
  const html = pageHtml({ title: "Recording", scripts: ["replay.js"], styles: ["player.css"], data: { events: "/x?a=1&b=<2>" } });
  assert.match(html, /data-events="\/x\?a=1&amp;b=&lt;2&gt;"/, "attributes are escaped");
  assert.doesNotMatch(html, /posthog|rrweb/i);
  assert.deepEqual(findings, [], lint.formatFindings(findings));
});

test("A3 commits touch only A3's paths: no console manifest, console lib, Insights, flag store, A1 or A2 files", () => {
  const allowed = [
    /^apps\/product-analytics\/src\/(replay|heatmaps|surveys|experiments|exports)\//,
    /^apps\/product-analytics\/src\/remote-config\/sections\.ts$/,
    /^apps\/product-analytics\/src\/flags\/variants\.ts$/,
    /^apps\/product-analytics\/public\/player\//,
    /^apps\/product-analytics\/db\/migrations\/002\d_[a-z0-9_]+\.sql$/,
    /^apps\/product-analytics\/test\/a3\//,
    /^apps\/console\/app\/\(app\)\/product-analytics\/(replay|heatmaps|surveys|experiments|exports)\//,
    /^apps\/console\/app\/\(app\)\/product-analytics\/_lib\/a3-client\.ts$/,
    /^docs\/analytics\/(oss\/A3\.md|CAPACITY\.md)$/,
  ];
  const commits = execFileSync("git", ["-C", RY, "log", "--format=%H", "--grep=(A3)", "--fixed-strings"], { encoding: "utf8" }).trim().split("\n").filter(Boolean);
  assert.ok(commits.length >= 5, `found ${commits.length} A3 commits`);
  for (const c of commits) {
    const files = execFileSync("git", ["-C", RY, "show", "--name-only", "--format=", c], { encoding: "utf8" }).trim().split("\n").filter(Boolean);
    for (const f of files) assert.ok(allowed.some((r) => r.test(f)), `${c.slice(0, 8)} touches ${f}`);
  }
  const dirty = execFileSync("git", ["-C", RY, "status", "--porcelain", "--", "apps/console/package.json", "apps/console/lib", "apps/insights/src", "packages/insights-web", "packages/insights-react-native", "apps/console/app/(app)/insights"], { encoding: "utf8" }).trim();
  assert.equal(dirty, "");
});
