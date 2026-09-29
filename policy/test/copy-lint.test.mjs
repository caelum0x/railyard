// Vendored copy-denylist lint for Railyard (Y-0). Run: node --test policy/test
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DENYLIST_PATH, PINNED_VERSION, loadDenylist, compileRules, selfTest, lintUnits, markdownUnits, errors,
} from "../copy-lint.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SOURCE = join(HERE, "../../../gtm/policy/copy-denylist.json");
const denylist = loadDenylist();

test("vendored denylist is the pinned version", () => {
  assert.equal(denylist.version, PINNED_VERSION);
  assert.ok(denylist.products.includes("railyard"));
});

test("vendored denylist matches gtm/policy byte for byte (when the source is on disk)", (t) => {
  if (!existsSync(SOURCE)) return t.skip("gtm/policy not present in this checkout");
  assert.equal(readFileSync(DENYLIST_PATH, "utf8"), readFileSync(SOURCE, "utf8"));
});

test("self-test: every rule's hit and miss examples behave in JavaScript (section 4.6)", () => {
  for (const product of denylist.products) {
    assert.deepEqual(selfTest(compileRules(denylist, { product })), [], product);
  }
});

test("each phrase added in 2026-09-29.2 has a failing fixture", () => {
  const fx = JSON.parse(readFileSync(join(HERE, "fixtures/denylist-2-new-phrases.json"), "utf8"));
  assert.equal(fx.version, "2026-09-29.2");
  for (const f of fx.fixtures) {
    const found = lintUnits([{ text: f.text, surface: f.surface, where: "fixture" }], { denylist, product: f.product });
    assert.ok(found.some((x) => x.id === f.rule), `${f.rule} should fail ${JSON.stringify(f.text)} (${f.product}/${f.surface})`);
  }
  const newRules = denylist.rules.filter((r) => r.id === "pre-warmed" || r.id.startsWith("railyard-") || r.id === "competitor-railyard");
  for (const r of newRules) {
    assert.ok(fx.fixtures.some((f) => f.rule === r.id), `rule ${r.id} has a fixture`);
  }
});

test("each rule added in 2026-09-29.3 has a failing fixture (PY1.0)", () => {
  const fx = JSON.parse(readFileSync(join(HERE, "fixtures/denylist-3-new-phrases.json"), "utf8"));
  assert.equal(fx.version, PINNED_VERSION);
  for (const f of fx.fixtures) {
    const found = lintUnits([{ text: f.text, surface: f.surface, where: "fixture" }], { denylist, product: f.product });
    assert.ok(found.some((x) => x.id === f.rule), `${f.rule} should fail ${JSON.stringify(f.text)} (${f.product}/${f.surface})`);
  }
  const added = ["portfolio-ott-drm", "portfolio-ott-comparison", "addon-unlaunched", "rally-capacity-purchase", "at-cost"];
  for (const id of added) {
    assert.ok(denylist.rules.some((r) => r.id === id), `rule ${id} is in the denylist`);
    assert.ok(fx.fixtures.some((f) => f.rule === id), `rule ${id} has a fixture`);
  }
});

test("2026-09-29.3 only adds: the new flags switch their rules off, and at cost passes next to a live source", () => {
  const addon = [{ text: "Add the portfolio add-on to Pro", surface: "marketing" }];
  assert.equal(errors(lintUnits(addon, { denylist, product: "scribase" })).length, 1);
  assert.equal(errors(lintUnits(addon, { denylist, product: "scribase", flags: { ADDON_LIVE: "1" } })).length, 0);
  const buy = [{ text: "Buy domains and mailboxes in one click", surface: "marketing" }];
  assert.equal(errors(lintUnits(buy, { denylist, product: "rally" })).length, 1);
  assert.equal(errors(lintUnits(buy, { denylist, product: "rally", flags: { RALLY_CAPACITY_PURCHASE_LIVE: "true" } })).length, 0);
  const drm = [{ text: "DRM for every video you ship", surface: "marketing" }];
  assert.equal(errors(lintUnits(drm, { denylist, flags: { OTT_DRM_LIVE: "1" } })).length, 0);
  const sourced = [{ text: 'Domains at cost <span data-copy-source="https://api.example/prices">$9.15</span>', surface: "marketing" }];
  assert.equal(errors(lintUnits(sourced, { denylist, product: "rally" })).length, 0);
  // The add-on card in the signed-in console is not an unlaunched public claim.
  const card = [{ text: "Portfolio add-on", surface: "app-ui" }];
  assert.equal(errors(lintUnits(card, { denylist, product: "scribase" })).length, 0);
});

test("a flag that is on in the real config switches its rule off", () => {
  const unit = [{ text: "Feature flags for every app", surface: "marketing" }];
  assert.equal(errors(lintUnits(unit, { denylist })).length, 1);
  assert.equal(errors(lintUnits(unit, { denylist, flags: { RAILYARD_FLAGS_LIVE: "1" } })).length, 0);
  const pw = [{ text: "Send from pre-warmed mailboxes", surface: "marketing" }];
  assert.equal(errors(lintUnits(pw, { denylist, product: "rally", flags: { MANAGED_SENDING_LIVE: "true" } })).length, 0);
});

test("honest Railyard copy passes", () => {
  const copy = [
    "Ship it, see who uses it, from your coding agent.",
    "Deploys to your own Cloudflare account. We host nothing.",
    "Your provider bills you directly.",
    "Charges go to your own payment account.",
    "Flags: arriving",
    "One command: npx railyard deploy",
  ];
  const found = lintUnits(copy.map((text) => ({ text, surface: "marketing" })), { denylist });
  assert.deepEqual(errors(found), []);
});

test("secret-surface findings never echo the matched text (section 4.5)", () => {
  const [f] = lintUnits([{ text: "use RELAY_API_KEY here", surface: "app-ui" }], { denylist });
  assert.equal(f.id, "env-var-names");
  assert.equal(f.excerpt, "");
});

test("markdown: code fences and inline code are not copy", () => {
  const md = "# Title\n\n```sh\nexport RELAY_API_KEY=x\n```\n\nRun `RELAY_API_KEY` never.\n\nPowered by Umami\n";
  const found = lintUnits(markdownUnits(md), { denylist });
  assert.deepEqual(found.map((f) => f.id), ["railyard-engine-names"]);
});

test("allowPaths skip a rule for its paths", () => {
  const unit = { text: "A guarantee we keep", surface: "marketing", path: "app/legal/terms/page.tsx" };
  assert.equal(lintUnits([unit], { denylist }).length, 0);
  assert.equal(lintUnits([{ ...unit, path: "app/page.tsx" }], { denylist }).length, 1);
});
