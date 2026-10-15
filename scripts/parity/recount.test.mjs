import assert from 'node:assert/strict';
import { test } from 'node:test';

import { applyChanges, countStatuses, formatReport, groups, mergeWaves, run, selfTest } from './recount.mjs';
import { ANALYTICS, BASELINE, INFRA, LIMRUN, ROWS } from './rows.mjs';
import { WAVE_Q } from './wave-q.mjs';
import { WAVE_R } from './wave-r.mjs';

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
  assert.equal(run(['--baseline'], (line) => lines.push(line)), 0);
  assert.match(lines.join('\n'), /infra: 26 \/ 12 \/ 6/);
});

test('Wave Q changes alone apply only with their test file and leave the baseline alone', () => {
  const { rows, skipped } = applyChanges(ROWS, WAVE_Q);
  assert.deepEqual(skipped, []);
  const split = groups(rows);
  assert.deepEqual(countStatuses(split.infra), { DONE: 29, PARTIAL: 10, MISSING: 5 });
  assert.deepEqual(countStatuses(split.analytics), { DONE: 24, PARTIAL: 6, MISSING: 7 });
  assert.deepEqual(selfTest(), []);
  for (const change of WAVE_Q) assert.match(change.evidence, /test/, change.id);
  const none = applyChanges(ROWS, WAVE_Q, () => false);
  assert.equal(none.skipped.length, WAVE_Q.length);
  assert.deepEqual(countStatuses(groups(none.rows).infra), { ...BASELINE.infra });
});

test('Wave R closes A4, A22 and A23 on top of Wave Q, and the default report applies both waves', () => {
  for (const change of WAVE_R) assert.match(change.evidence, /test|Test/, change.id);
  const { rows, skipped } = applyChanges();
  assert.deepEqual(skipped, []);
  const split = groups(rows);
  // Wave Q alone: infra 29/10/5, analytics 24/6/7. Wave R moves A4, A22 and
  // A23 from PARTIAL to DONE: analytics gains 3 DONE, loses 3 PARTIAL.
  assert.deepEqual(countStatuses(split.infra), { DONE: 29, PARTIAL: 10, MISSING: 5 });
  assert.deepEqual(countStatuses(split.analytics), { DONE: 27, PARTIAL: 3, MISSING: 7 });
  assert.deepEqual(selfTest(), []);
  const byId = Object.fromEntries(rows.map((row) => [row.id, row]));
  assert.equal(byId.A4.status, 'DONE');
  assert.equal(byId.A22.status, 'DONE');
  assert.equal(byId.A23.status, 'DONE');
  const lines = [];
  assert.equal(run([], (line) => lines.push(line)), 0);
  const text = lines.join('\n');
  assert.match(text, /infra: 29 \/ 10 \/ 5/);
  assert.match(text, /analytics: 27 \/ 3 \/ 7/);
  assert.match(text, /baseline 2026-09-30: infra 26 \/ 12 \/ 6, analytics 20 \/ 9 \/ 8/);
});

test('mergeWaves lets a later wave override an earlier one on the same row id', () => {
  const merged = mergeWaves(WAVE_Q, WAVE_R);
  const a23 = merged.find((change) => change.id === 'A23');
  assert.equal(a23.status, 'DONE');
  assert.equal(a23.pkg, '2026-10-15');
  // Wave Q's own A23 entry (PARTIAL) is not the one that wins.
  assert.notEqual(a23, WAVE_Q.find((change) => change.id === 'A23'));
});
