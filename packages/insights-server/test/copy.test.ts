// The Insights depth docs and messages (PY3.2) pass the vendored copy denylist.
// Profiles and cohorts are claims only with RAILYARD_REPLAY_LIVE on (rule
// railyard-slot-replay); the console serves them only then (checked below), so
// these docs are linted as live, and every other rule still applies.
//   node --test test/*.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { errors, formatFindings, lintUnits, markdownUnits } from '../../../policy/copy-lint.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '../../..');
const R = (p: string) => readFileSync(join(ROOT, p), 'utf8');
const strings = (src: string) =>
  [...src.replace(/^\s*(\/\/|\*).*$/gm, '').matchAll(/["'`]([^"'`\n]{12,})["'`]/g)].map((m) => m[1] as string).filter((s) => !/^[\^[/.]|^ry_|https?:|\$\{|^[a-z_]+\.[a-z]/.test(s));
const LIVE = { RAILYARD_REPLAY_LIVE: '1' };

test('package READMEs pass the copy lint (docs surface, replay live)', () => {
  const units = ['packages/insights-server/README.md', 'packages/insights-react-native/README.md'].flatMap((f) => markdownUnits(R(f), { where: f }));
  const found = errors(lintUnits(units, { flags: LIVE }));
  assert.deepEqual(found, [], formatFindings(found));
});

test('messages people can read pass the copy lint and name no upstream product', () => {
  const files: [string, string, Record<string, string>][] = [
    ['packages/insights-server/src/index.ts', 'api-public', LIVE],
    ['packages/insights-react-native/src/index.ts', 'api-public', LIVE],
    ['apps/console/lib/insights-depth/forward.ts', 'api-public', {}],
    ['apps/insights/src/railyard/people.ts', 'api-public', LIVE],
    ['apps/insights/src/railyard/cohorts.ts', 'api-public', LIVE],
    ['apps/insights/src/railyard/analysis.ts', 'api-public', LIVE],
    ['apps/insights/src/railyard/export.ts', 'api-public', LIVE],
  ];
  for (const [f, surface, flags] of files) {
    const units = strings(R(f)).map((text) => ({ text, surface, where: f }));
    const found = errors(lintUnits(units, { flags }));
    assert.deepEqual(found, [], formatFindings(found));
    assert.doesNotMatch(R(f).replace(/^\s*(\/\/|\*).*$/gm, ''), /umami/i, f);
  }
});

test('the console serves Insights depth only with RAILYARD_REPLAY_LIVE on', () => {
  const src = R('apps/console/lib/insights-depth/forward.ts');
  assert.match(src, /if \(!deps\.live\) return reply\(\{ error: ARRIVING \}, 503\)/);
  assert.match(R('apps/console/lib/config.ts'), /replayLive: env\.RAILYARD_REPLAY_LIVE === "1"/);
  assert.match(R('apps/console/app/api/v1/insights/[...path]/route.ts'), /live: cfg\.replayLive/);
});
