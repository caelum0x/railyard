// Vendored open-source files: unchanged since they were copied (sha256), each
// with its licence, and the SDK parts the service serves at /static/ equal
// the SDK version the @railyard/analytics-js wrapper installs.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SVC = join(dirname(fileURLToPath(import.meta.url)), "../..");
const sha = (p: string) => createHash("sha256").update(readFileSync(join(SVC, p))).digest("hex").slice(0, 16);

const PINNED: Record<string, string> = {
  "public/player/vendor/rrweb-player/rrweb-player.min.js": "c73939967f282003",
  "public/player/vendor/rrweb-player/style.css": "760311ccb2da5389",
  "public/player/vendor/simpleheat/simpleheat.js": "5ce82dbd979299ff",
  "public/player/sdk/lazy-recorder.js": "e0ba04d3a8640360",
  "public/player/sdk/surveys.js": "a0a5ccf176f925c1",
  "public/player/sdk/dead-clicks-autocapture.js": "329a35bc14da77ed",
  "src/replay/vendor/aws4fetch/aws4fetch.esm.mjs": "3049cd99d00a2fb8",
  "src/experiments/vendor/jstat/jstat.cjs": "0f259c0087245bba",
};

test("vendored files are unchanged and keep their licences", () => {
  for (const [p, h] of Object.entries(PINNED)) assert.equal(sha(p), h, p);
  for (const d of ["public/player/vendor/rrweb-player", "public/player/vendor/simpleheat", "public/player/sdk", "src/replay/vendor/aws4fetch", "src/experiments/vendor/jstat"]) {
    assert.ok(existsSync(join(SVC, d, "LICENSE")), `${d}/LICENSE`);
    const pkg = JSON.parse(readFileSync(join(SVC, d, "package.json"), "utf8"));
    assert.match(JSON.stringify(pkg.license ?? pkg.licenses), /MIT|BSD-2-Clause|Apache-2\.0/, d);
  }
  for (const n of ["public/player/NOTICE", "src/replay/NOTICE", "src/experiments/NOTICE"]) assert.ok(existsSync(join(SVC, n)), n);
});

test("the SDK parts served at /static/ are the wrapper's SDK version", { skip: existsSync(join(SVC, "../../packages/analytics-js/node_modules/posthog-js/package.json")) ? false : "wrapper dependencies not installed" }, () => {
  const wrapper = join(SVC, "../../packages/analytics-js/node_modules/posthog-js");
  const version = JSON.parse(readFileSync(join(wrapper, "package.json"), "utf8")).version;
  assert.equal(JSON.parse(readFileSync(join(SVC, "public/player/sdk/package.json"), "utf8")).version, version);
  for (const f of ["lazy-recorder.js", "surveys.js", "dead-clicks-autocapture.js"]) assert.ok(readFileSync(join(wrapper, "dist", f)).equals(readFileSync(join(SVC, "public/player/sdk", f))), f);
});
