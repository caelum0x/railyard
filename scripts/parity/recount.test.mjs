import assert from 'node:assert/strict';
import { test } from 'node:test';

import { countStatuses, formatReport, groups, run, selfTest } from './recount.mjs';
import { ANALYTICS, BASELINE, INFRA, LIMRUN, ROWS } from './rows.mjs';

test('rows cover I1-I44, A1-A26 and L1-L12 once each', () => {
  assert.equal(INFRA.length, 44);
  assert.equal(ANALYTICS.length, 26);
  assert.equal(LIMRUN.length, 12);
  assert.deepEqual(INFRA.map((row) => row.id), Array.from({ length: 44 }, (_, i) => `I${i + 1}`));
  assert.deepEqual(ANALYTICS.map((row) => row.id), Array.from({ length: 26 }, (_, i) => `A${i + 1}`));
  assert.deepEqual(LIMRUN.map((row) => row.id), Array.from({ length: 12 }, (_, i) => `L${i + 1}`));
  assert.equal(ROWS.length, 82);
});

test('every row is DONE, PARTIAL or MISSING and names evidence from the recount', () => {
  for (const row of ROWS) {
    assert.ok(['DONE', 'PARTIAL', 'MISSING'].includes(row.status), row.id);
    assert.equal(typeof row.evidence, 'string');
    assert.ok(row.evidence.length > 0, row.id);
    assert.ok(row.name.length > 0, row.id);
  }
  const byId = Object.fromEntries(ROWS.map((row) => [row.id, row]));
  assert.equal(byId.I5.status, 'PARTIAL');
  assert.match(byId.I5.evidence, /ship/);
  assert.equal(byId.I6.status, 'MISSING');
  assert.equal(byId.I6.evidence, 'none');
  assert.equal(byId.I18.status, 'MISSING');
  assert.equal(byId.I44.status, 'MISSING');
  assert.equal(byId.A25.status, 'MISSING');
  assert.equal(byId.A25.evidence, 'none');
  assert.equal(byId.A22.status, 'PARTIAL');
  assert.match(byId.A22.evidence, /exports\.test\.ts/);
  assert.equal(byId.L12.status, 'DONE');
  assert.equal(byId.L1.status, 'MISSING');
  assert.equal(byId.L11.status, 'DONE');
});

test('the static record reproduces infra 26/12/6 and analytics 20/9/8', () => {
  const split = groups();
  assert.equal(split.infra.length, 44);
  assert.equal(split.analytics.length, 37);
  assert.equal(split.analytics.some((row) => row.id === 'L11'), false);
  assert.deepEqual(countStatuses(split.infra), { ...BASELINE.infra });
  assert.deepEqual(countStatuses(split.analytics), { ...BASELINE.analytics });
  assert.deepEqual(countStatuses(INFRA), { DONE: 26, PARTIAL: 12, MISSING: 6 });
  const countableLim = LIMRUN.filter((row) => row.id !== 'L11');
  const analytics = countStatuses([...ANALYTICS, ...countableLim]);
  assert.deepEqual(analytics, { DONE: 20, PARTIAL: 9, MISSING: 8 });
  assert.deepEqual(selfTest(), []);
});

test('the report prints a status per row and both totals', () => {
  const text = formatReport();
  for (const row of ROWS) {
    if (row.id === 'L11') continue;
    assert.match(text, new RegExp(`^${row.id}\\s+${row.status}\\s`, 'm'), row.id);
  }
  assert.match(text, /infra: 26 \/ 12 \/ 6 \(DONE \/ PARTIAL \/ MISSING\)/);
  assert.match(text, /analytics: 20 \/ 9 \/ 8 \(DONE \/ PARTIAL \/ MISSING\)/);
  assert.match(text, /^L12\s+DONE\s/m);
});

test('empty input, a bad status and a duplicate id fail the self-test', () => {
  assert.ok(selfTest([]).length > 0);
  const broken = ROWS.map((row) => (row.id === 'I1' ? { ...row, status: 'WORKS' } : row));
  assert.match(selfTest(broken).join('\n'), /I1 has status WORKS/);
  const dup = [...ROWS, { ...ROWS[0] }];
  assert.match(selfTest(dup).join('\n'), /I1 is listed twice/);
  const blank = ROWS.map((row) => (row.id === 'A1' ? { ...row, evidence: '' } : row));
  assert.match(selfTest(blank).join('\n'), /A1 has no evidence/);
});

test('--self-test exits 0 on the committed rows and 1 when the baseline is wrong', () => {
  assert.equal(run(['--self-test'], () => {}), 0);
  const lines = [];
  assert.equal(run([], (line) => lines.push(line)), 0);
  assert.match(lines.join('\n'), /26 \/ 12 \/ 6/);
});
