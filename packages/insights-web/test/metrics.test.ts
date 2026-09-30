// Page-speed metrics (Wave P PY3.3): the numbers for each fixture page load
// are within 10% of what Chrome's field reporting gives for the same entries,
// and so is the 75th percentile over all of them (the figure the views show).
//   node --test test/*.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { computeVitals, p75, type PerfEntry, type Vitals } from '../src/metrics.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(readFileSync(join(HERE, 'fixtures/chrome-page-loads.json'), 'utf8')) as {
  loads: { name: string; entries: PerfEntry[]; chrome: Vitals; interactionCount?: number }[];
};
const METRICS = ['lcp', 'inp', 'cls', 'fcp', 'ttfb'] as const;
const within10 = (got: number | undefined, want: number) => got !== undefined && (want === 0 ? got === 0 : Math.abs(got - want) / want <= 0.1);

for (const load of fixture.loads) {
  test(`within 10% of Chrome: ${load.name}`, () => {
    const got = computeVitals(load.entries, { interactionCount: load.interactionCount, layoutShiftSupported: true });
    for (const m of METRICS) {
      const want = load.chrome[m];
      if (want === undefined) assert.equal(got[m], undefined, `${m} should be absent`);
      else assert.ok(within10(got[m], want), `${m}: got ${got[m]}, Chrome ${want}`);
    }
  });
}

test('the p75 over all loads is within 10% of the p75 of the Chrome values', () => {
  const ours = fixture.loads.map((l) => computeVitals(l.entries, { interactionCount: l.interactionCount, layoutShiftSupported: true }));
  for (const m of METRICS) {
    const chrome = fixture.loads.map((l) => l.chrome[m]).filter((v): v is number => v !== undefined);
    const mine = ours.map((v) => v[m]).filter((v): v is number => v !== undefined);
    assert.equal(mine.length, chrome.length, m);
    assert.ok(within10(p75(mine) ?? undefined, p75(chrome) as number), `${m}: p75 ${p75(mine)} vs ${p75(chrome)}`);
  }
});

test('without the two corrections the LCP and INP cases would be far off (the fixture is not trivial)', () => {
  const lcpCase = fixture.loads[1]!;
  const lastCandidate = Math.max(...lcpCase.entries.filter((e) => e.entryType === 'largest-contentful-paint').map((e) => e.startTime));
  assert.ok(!within10(lastCandidate, lcpCase.chrome.lcp as number), 'the paint after the click is 4x the real LCP');
  const inpCase = fixture.loads[4]!;
  const slowest = Math.max(...inpCase.entries.filter((e) => e.interactionId).map((e) => e.duration));
  assert.ok(!within10(slowest, inpCase.chrome.inp as number));
  // A browser without layout-shift reporting gives no CLS rather than a made-up 0.
  assert.equal(computeVitals(fixture.loads[3]!.entries).cls, undefined);
});
