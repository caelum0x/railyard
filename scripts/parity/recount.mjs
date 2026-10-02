#!/usr/bin/env node
/**
 * Railyard parity recount (Wave Q, QY1.0).
 *
 * Prints DONE, PARTIAL or MISSING for every row in scripts/parity/rows.mjs.
 * The statuses are the static record of PARITY-RECOUNT-2026-09-30.md section 5
 * on today's main (ae0f113). The script does not re-run the product tests.
 *
 *   node scripts/parity/recount.mjs
 *   node scripts/parity/recount.mjs --self-test
 *
 * --self-test checks that the printed totals are infra 26/12/6 and analytics
 * 20/9/8 (A1-A26 plus L1-L12 except L11, which the recount marks n/a).
 */
import { fileURLToPath } from 'node:url';

import { ANALYTICS, BASELINE, INFRA, LIMRUN, ROWS } from './rows.mjs';

export const STATUSES = Object.freeze(['DONE', 'PARTIAL', 'MISSING']);

/** @param {readonly {status: string}[]} rows */
export function countStatuses(rows) {
  const counts = { DONE: 0, PARTIAL: 0, MISSING: 0 };
  for (const row of rows) {
    if (!Object.hasOwn(counts, row.status)) {
      throw new Error(`unknown status ${row.status}`);
    }
    counts[row.status] += 1;
  }
  return counts;
}

/**
 * Infra is I1-I44. Analytics follows the recount: A1-A26 plus lim.run rows
 * except L11 (G11), which is n/a and not in the 37.
 */
export function groups() {
  return {
    infra: INFRA,
    analytics: [...ANALYTICS, ...LIMRUN.filter((row) => row.id !== 'L11')],
  };
}

/** @param {{ DONE: number, PARTIAL: number, MISSING: number }} got @param {{ DONE: number, PARTIAL: number, MISSING: number }} want */
function sameCounts(got, want) {
  return got.DONE === want.DONE && got.PARTIAL === want.PARTIAL && got.MISSING === want.MISSING;
}

/** Problems with the registry. Empty when the baseline holds. */
export function selfTest(rows = ROWS) {
  const problems = [];
  const seen = new Set();
  for (const row of rows) {
    if (seen.has(row.id)) problems.push(`${row.id} is listed twice`);
    seen.add(row.id);
    if (!STATUSES.includes(row.status)) problems.push(`${row.id} has status ${row.status}`);
    if (typeof row.evidence !== 'string' || row.evidence.length === 0) {
      problems.push(`${row.id} has no evidence`);
    }
    if (typeof row.name !== 'string' || row.name.length === 0) problems.push(`${row.id} has no name`);
  }
  const wantIds = [
    ...Array.from({ length: 44 }, (_, i) => `I${i + 1}`),
    ...Array.from({ length: 26 }, (_, i) => `A${i + 1}`),
    ...Array.from({ length: 12 }, (_, i) => `L${i + 1}`),
  ];
  for (const id of wantIds) {
    if (!seen.has(id)) problems.push(`missing row ${id}`);
  }
  if (rows.length !== wantIds.length) problems.push(`expected ${wantIds.length} rows, got ${rows.length}`);

  const split = groups();
  const infra = countStatuses(split.infra);
  const analytics = countStatuses(split.analytics);
  if (!sameCounts(infra, BASELINE.infra)) {
    problems.push(`infra is ${infra.DONE}/${infra.PARTIAL}/${infra.MISSING}, want 26/12/6`);
  }
  if (!sameCounts(analytics, BASELINE.analytics)) {
    problems.push(`analytics is ${analytics.DONE}/${analytics.PARTIAL}/${analytics.MISSING}, want 20/9/8`);
  }
  if (split.analytics.length !== 37) problems.push(`analytics countable rows are ${split.analytics.length}, want 37`);
  const l11 = rows.find((row) => row.id === 'L11');
  if (!l11 || l11.note?.includes('n/a') !== true) problems.push('L11 must stay n/a and out of the analytics total');
  return problems;
}

/** One line per row, then the two totals. */
export function formatReport(rows = ROWS) {
  const lines = [];
  const split = groups();
  const printable = [
    ['infra', split.infra],
    ['analytics', split.analytics],
    ['limrun n/a', LIMRUN.filter((row) => row.id === 'L11')],
  ];
  for (const [label, list] of printable) {
    lines.push(`== ${label}`);
    for (const row of list) {
      const note = row.note ? `  (${row.note})` : '';
      lines.push(`${row.id.padEnd(4)} ${row.status.padEnd(8)} ${row.name}  [${row.evidence}]${note}`);
    }
    if (label === 'limrun n/a') continue;
    const counts = countStatuses(list);
    lines.push(`${label}: ${counts.DONE} / ${counts.PARTIAL} / ${counts.MISSING} (DONE / PARTIAL / MISSING)`);
  }
  return lines.join('\n');
}

export function run(argv = process.argv.slice(2), log = (line) => process.stdout.write(`${line}\n`)) {
  if (argv.includes('--self-test')) {
    const problems = selfTest();
    for (const problem of problems) log(`SELF-TEST FAIL: ${problem}`);
    log(problems.length === 0 ? 'self-test ok' : 'self-test failed');
    return problems.length === 0 ? 0 : 1;
  }
  log(formatReport());
  const problems = selfTest();
  if (problems.length > 0) {
    for (const problem of problems) log(`recount: ${problem}`);
    return 1;
  }
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  process.exitCode = run();
}
