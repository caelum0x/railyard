// The web package docs and messages (PY3.3) pass the vendored copy denylist.
// Page-speed copy is allowed with RAILYARD_REPLAY_LIVE on (rule
// railyard-slot-replay); the views that show it check that flag first
// (apps/console/lib/vitals-view.test.ts), so the docs are linted as live.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { errors, formatFindings, lintUnits, markdownUnits } from '../../../policy/copy-lint.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const LIVE = { RAILYARD_REPLAY_LIVE: '1' };

test('README and messages pass the copy lint; the upstream name appears only in NOTICE', () => {
  const units = [
    ...markdownUnits(readFileSync(join(ROOT, 'README.md'), 'utf8'), { where: 'README.md' }),
    ...['src/index.ts', 'src/metrics.ts'].flatMap((f) =>
      [...readFileSync(join(ROOT, f), 'utf8').replace(/^\s*(\/\/|\*).*$/gm, '').matchAll(/["'`]([^"'`\n]{12,})["'`]/g)]
        .map((m) => m[1] as string)
        .filter((s) => !/^[\^[/.]|https?:|\$\{/.test(s))
        .map((text) => ({ text, surface: 'docs', where: f })),
    ),
  ];
  const found = errors(lintUnits(units, { flags: LIVE }));
  assert.deepEqual(found, [], formatFindings(found));
  for (const f of ['README.md', 'src/index.ts', 'src/metrics.ts', 'package.json']) assert.doesNotMatch(readFileSync(join(ROOT, f), 'utf8'), /umami/i, f);
  assert.match(readFileSync(join(ROOT, 'NOTICE'), 'utf8'), /MIT License/);
});
