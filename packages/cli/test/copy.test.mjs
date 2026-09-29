// The README and every string the CLI prints pass the vendored copy denylist (docs surface).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { errors, formatFindings, lintUnits, markdownUnits } from "../../../policy/copy-lint.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

test("README passes the copy lint", () => {
  const found = errors(lintUnits(markdownUnits(readFileSync(join(ROOT, "README.md"), "utf8"), { where: "README.md" })));
  assert.deepEqual(found, [], formatFindings(found));
});

test("CLI output strings pass the copy lint", () => {
  const units = readdirSync(join(ROOT, "src")).flatMap((f) => {
    const src = readFileSync(join(ROOT, "src", f), "utf8");
    return [...src.matchAll(/(["`])((?:(?!\1)[^\\\n]|\\.){12,})\1/g)].filter((m) => !/^@?[a-z0-9./-]+@[\d.]+$/.test(m[2])) // package specifiers are commands, not copy
      .map((m) => ({ text: m[2].replace(/\$\{[^}]*\}/g, "x"), surface: "docs", where: f }));
  });
  assert.ok(units.length > 20);
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
});
