// The Watch screen, the public status page, the alert email and the watch
// messages pass the vendored copy denylist, and name no host or engine.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { errors, formatFindings, lintUnits } from "../../../policy/copy-lint.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const R = (p: string) => readFileSync(join(ROOT, p), "utf8");
const strings = (src: string) =>
  [...src.replace(/^\s*\/\/.*$/gm, "").matchAll(/["`]([^"`\n]{12,})["`]|>([^<>{}\n]{12,})</g)]
    .map((m) => (m[1] ?? m[2]).trim())
    .filter((s) => !/^[\^[/.]|https?:|\$\{|^[a-z-]+:|^watch_|^railyard /.test(s));

// Watch copy is allowed only with RAILYARD_WATCH_LIVE on (denylist rule
// railyard-slot-watch). Every file listed "live" is reachable only behind that
// flag (the checks below prove each one tests it); the rest must pass with it off.
const LIVE = { RAILYARD_WATCH_LIVE: "1" };
for (const [file, surface, flags] of [
  ["app/status/[app]/page.tsx", "marketing", LIVE],
  ["app/(app)/watch/page.tsx", "app-ui", LIVE],
  ["components/WatchPanel.tsx", "app-ui", LIVE],
  ["lib/watch/service.ts", "outbound", LIVE],
  ["lib/watch/http.ts", "api-public", {}],
  ["components/Nav.tsx", "app-ui", {}],
] as const) {
  test(`${file} passes the copy lint (${surface}${flags === LIVE ? ", Watch live" : ""})`, () => {
    const units = strings(R(file)).map((text) => ({ text, surface, where: file }));
    const found = errors(lintUnits(units, { flags }));
    assert.deepEqual(found, [], formatFindings(found));
    for (const u of units) assert.doesNotMatch(u.text, /gatus|ereboslab|box ?1|hetzner|contabo/i, `${file}: ${u.text}`);
  });
}

test("every Watch surface linted as live checks the flag first", () => {
  assert.match(R("app/status/[app]/page.tsx"), /if \(!watch\.live/);
  assert.match(R("app/(app)/watch/page.tsx"), /if \(!watch\.live\)/);
  assert.match(R("app/(app)/layout.tsx"), /<Nav watch=\{deps\(\)\.cfg\.watchLive\}/);
  const svc = R("lib/watch/service.ts");
  for (const fn of ["createCheck", "agentChecks", "recordResults"]) {
    assert.match(svc, new RegExp(`function ${fn}[^]*?if \\(!deps\\.live\\)`), fn);
  }
});
