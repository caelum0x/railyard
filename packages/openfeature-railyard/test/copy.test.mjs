// The flags package docs and every customer-facing flags string (the console
// screen, its messages and the API errors) pass the vendored copy denylist.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { errors, formatFindings, lintUnits, markdownUnits } from "../../../policy/copy-lint.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const R = (p) => readFileSync(join(HERE, "..", p), "utf8");
const strings = (src) => [...src.matchAll(/["`]([^"`\n]{12,})["`]/g)].map((m) => m[1]).filter((s) => !/^[\^[/.]|^ry_|https?:|\$\{/.test(s));

test("README and examples pass the copy lint (docs surface)", () => {
  const units = [
    ...markdownUnits(R("README.md"), { where: "README.md" }),
    ...["examples/expo/App.tsx", "examples/next/app/page.tsx", "src/client.mjs", "src/provider.mjs"].flatMap((f) => strings(R(f)).map((text) => ({ text, surface: "docs", where: f }))),
  ];
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
});

test("the console Flags screen and the flags messages pass the copy lint (app-ui surface)", () => {
  const files = [
    "../../apps/console/app/(app)/flags/page.tsx",
    "../../apps/console/components/FlagsPanel.tsx",
    "../../apps/console/lib/flags/service.ts",
    "../../apps/console/lib/flags/http.ts",
  ];
  const units = files.flatMap((f) => strings(R(f)).map((text) => ({ text, surface: "app-ui", where: f })));
  assert.ok(units.length > 10);
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
  for (const f of files) assert.doesNotMatch(R(f).replace(/^\/\/.*$/gm, ""), /Flagsmith/, `${f}: no engine name outside comments`);
});
