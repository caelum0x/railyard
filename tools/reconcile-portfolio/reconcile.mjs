#!/usr/bin/env node
// Nightly reconciliation for the Railyard consumer contract (Wave P PY1.5):
// each product's own counts against Rago's totals, per customer, meter and
// UTC month. A difference other than 0 exits 1 and names every row.
//
//   node tools/reconcile-portfolio/reconcile.mjs --product counts.json --rago totals.json
//
// Inputs are exports, one row per (customer, meter, month):
//   product: [{ "external_customer_id", "code", "month": "YYYY-MM", "value" }]
//     (the product's own table; each lane exports it from its database)
//   rago:    same shape, from Rago's `events`:
//     select external_customer_id, code, to_char(timestamp, 'YYYY-MM') as month,
//            sum(coalesce((properties->>'<field>')::numeric, 1)) as value
//     from events where code like '<product>.%' and deleted_at is null group by 1, 2, 3;
// Read-only: nothing is written anywhere but stdout.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const key = (r) => `${r.external_customer_id}\u0000${r.code}\u0000${r.month}`;

function validate(rows, label) {
  if (!Array.isArray(rows)) throw new Error(`${label}: expected an array of rows`);
  for (const r of rows) {
    if (typeof r.external_customer_id !== "string" || typeof r.code !== "string" || !/^\d{4}-\d{2}$/.test(r.month ?? "") || !Number.isFinite(Number(r.value))) {
      throw new Error(`${label}: malformed row ${JSON.stringify(r)}`);
    }
  }
  return rows;
}

function totals(rows) {
  const m = new Map();
  for (const r of rows) m.set(key(r), (m.get(key(r)) ?? 0) + Number(r.value));
  return m;
}

/** Rows whose totals differ (missing on either side counts as 0). */
export function reconcile(productRows, ragoRows) {
  const p = totals(validate(productRows, "product"));
  const g = totals(validate(ragoRows, "rago"));
  const diffs = [];
  for (const k of new Set([...p.keys(), ...g.keys()])) {
    const a = p.get(k) ?? 0;
    const b = g.get(k) ?? 0;
    if (Math.abs(a - b) > 1e-9) {
      const [external_customer_id, code, month] = k.split("\u0000");
      diffs.push({ external_customer_id, code, month, product: a, rago: b, difference: b - a });
    }
  }
  return diffs.sort((x, y) => (x.code + x.external_customer_id < y.code + y.external_customer_id ? -1 : 1));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const arg = (name) => {
    const i = process.argv.indexOf(name);
    return i > 0 ? process.argv[i + 1] : null;
  };
  const productPath = arg("--product");
  const ragoPath = arg("--rago");
  if (!productPath || !ragoPath) {
    process.stderr.write("usage: reconcile.mjs --product <counts.json> --rago <totals.json>\n");
    process.exit(2);
  }
  try {
    const diffs = reconcile(JSON.parse(readFileSync(productPath, "utf8")), JSON.parse(readFileSync(ragoPath, "utf8")));
    process.stdout.write(`${JSON.stringify({ difference_rows: diffs.length, diffs })}\n`);
    process.exit(diffs.length === 0 ? 0 : 1);
  } catch (e) {
    process.stderr.write(`reconcile-portfolio: ${e.message}\n`);
    process.exit(2);
  }
}
