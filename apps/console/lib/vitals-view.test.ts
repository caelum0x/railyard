// Page speed with deploy markers (Wave P PY3.3): the view model, the flag
// gate and the copy lint with the flag on. The bridge answer is tested in
// app/api/v1/bridge/scribase/bridge.test.ts.
//   node --test lib/vitals-view.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import type { Deploy } from "./types.ts";
import type { InsightsVitals, VitalsRow } from "./insights.ts";
import { format, markTimes, vitalsView } from "./vitals-view.ts";
import { errors, formatFindings, lintUnits } from "../../../policy/copy-lint.mjs";

const NOW = new Date("2026-09-30T12:00:00Z");
const APP = "5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa";
const row = (lcp: number, inp: number, cls: number): VitalsRow => ({
  app_id: APP,
  samples: 40,
  lcp: { p75: lcp, rating: lcp <= 2500 ? "good" : lcp > 4000 ? "poor" : "needs-improvement" },
  inp: { p75: inp, rating: "good" },
  cls: { p75: cls, rating: "good" },
  fcp: { p75: 900, rating: "good" },
  ttfb: { p75: 200, rating: "good" },
});
const deploy = (id: string, at: string, over: Partial<Deploy> = {}): Deploy => ({
  id, org_id: "o", app_id: APP, url: "https://shop.acme.workers.dev", commit_sha: null, branch: "main", framework: "vite", version_id: null, duration_ms: 1, status: "succeeded", rollback_of: null, created_at: at, ...over,
});

test("the view: p75 per metric with a plain rating, and per deploy what changed after it", () => {
  const d1 = deploy("d1", "2026-09-29T12:00:00.000Z");
  const v: InsightsVitals = {
    since: "2026-09-23T12:00:00.000Z",
    apps: [row(2600, 120, 0.05)],
    daily: [{ ...row(2600, 120, 0.05), day: "2026-09-29" }],
    deploys: [{ at: d1.created_at, before: row(1300, 120, 0.05), after: row(3750, 125, 0.2) }],
  };
  const view = vitalsView(v, APP, [d1]);
  assert.deepEqual(view.metrics.map((m) => [m.key, m.value, m.rating]), [
    ["lcp", "2600 ms", "needs work"],
    ["inp", "120 ms", "good"],
    ["cls", "0.05", "good"],
    ["fcp", "900 ms", "good"],
    ["ttfb", "200 ms", "good"],
  ]);
  assert.deepEqual(view.deploys[0]!.changes.slice(0, 3), [
    "Largest paint (LCP) 1300 ms to 3750 ms, slower",
    "Response to input (INP) 120 ms to 125 ms, about the same",
    "Layout shift (CLS) 0.05 to 0.20, more shifting",
  ]);
  assert.equal(view.deploys[0]!.worse, "cls");
  assert.equal(format("lcp", null), "no data");
  // Only this app's successful deploys in the window, newest first, at most 10.
  const many = [deploy("old", "2026-09-01T00:00:00Z"), deploy("fail", "2026-09-29T00:00:00Z", { status: "failed" }), deploy("other", "2026-09-29T00:00:00Z", { app_id: "x" }), ...Array.from({ length: 12 }, (_, i) => deploy(`n${i}`, `2026-09-2${Math.min(9, 4 + Math.floor(i / 2))}T0${i % 10}:00:00Z`))];
  const marks = markTimes(many, APP, new Date("2026-09-23T12:00:00Z"));
  assert.equal(marks.length, 10);
  assert.ok(marks.every((m) => m.status === "succeeded" && m.app_id === APP));
  assert.ok(marks[0]!.created_at >= marks[9]!.created_at);
});

test("the Page speed screen passes the copy lint with the flag on, and checks the flag first", () => {
  const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
  const src = readFileSync(join(ROOT, "app/(app)/insights/vitals/page.tsx"), "utf8");
  assert.match(src, /if \(!cfg\.replayLive \|\| !insights\?\.vitals\)/);
  const strings = [...src.matchAll(/["`]([^"`\n]{12,})["`]|>([^<>{}\n]{12,})</g)].map((m) => (m[1] ?? m[2])!.trim()).filter((s) => !/^[\^[/.]|https?:|\$\{|^[a-z-]+:|insights \//.test(s));
  const model = readFileSync(join(ROOT, "lib/vitals-view.ts"), "utf8");
  const modelStrings = [...model.matchAll(/"([^"\n]{12,})"/g)].map((m) => m[1]!);
  const units = [...strings, ...modelStrings].map((text) => ({ text, surface: "app-ui", where: "vitals" }));
  const found = errors(lintUnits(units, { flags: { RAILYARD_REPLAY_LIVE: "1" } }));
  assert.deepEqual(found, [], formatFindings(found));
});
