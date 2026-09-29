import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { plan, render } from "../provision-product-keys.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const contract = JSON.parse(readFileSync(join(HERE, "../../../tests/contract/consumers/contract.json"), "utf8"));

test("one plan per product with names only, never a value", () => {
  const p = plan(contract);
  assert.deepEqual(p.map((x) => x.product), ["scribase", "menivor", "rally"]);
  const text = render(p) + JSON.stringify(p);
  assert.doesNotMatch(text, /sk-t0-|whsec_|rago_live_|[A-Fa-f0-9]{32,}/);
  for (const x of p) assert.ok(x.vars.includes("RAILYARD_LIVE=0"), "nothing switches live by default");
  const out = execFileSync(process.execPath, [join(HERE, "../provision-product-keys.mjs"), "--json"], { encoding: "utf8" });
  assert.equal(JSON.parse(out).length, 3);
});
