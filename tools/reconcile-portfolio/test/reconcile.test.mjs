import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { reconcile } from "../reconcile.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const SCRIPT = join(HERE, "../reconcile.mjs");
const product = JSON.parse(readFileSync(join(HERE, "fixtures/product-counts.json"), "utf8"));
const rago = JSON.parse(readFileSync(join(HERE, "fixtures/rago-totals.json"), "utf8"));

test("on the fixtures Rago's totals equal each product's own counts: difference 0", () => {
  assert.deepEqual(reconcile(product, rago), []);
  const r = spawnSync(process.execPath, [SCRIPT, "--product", join(HERE, "fixtures/product-counts.json"), "--rago", join(HERE, "fixtures/rago-totals.json")], { encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(JSON.parse(r.stdout).difference_rows, 0);
});

test("a dropped event, an extra event and a missing row are each named", () => {
  const dropped = rago.filter((row) => row.code !== "rally.email.sent");
  const extra = [...rago, { external_customer_id: "menivor_org_0001", code: "menivor.reel.rendered", month: "2026-09", value: 1 }];
  assert.deepEqual(reconcile(product, dropped), [{ external_customer_id: "rally_org_0001", code: "rally.email.sent", month: "2026-09", product: 480, rago: 0, difference: -480 }]);
  assert.equal(reconcile(product, extra)[0].difference, 1);
  assert.throws(() => reconcile([{ code: "x" }], []), /malformed/);
});
