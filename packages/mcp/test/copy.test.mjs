// The plugin README, the MCP README, the skill and every tool description and
// message pass the vendored copy denylist (docs surface).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { errors, formatFindings, lintUnits, markdownUnits } from "../../../policy/copy-lint.mjs";
import { TOOLS } from "../src/tools.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const R = (p) => readFileSync(join(HERE, "..", p), "utf8");

test("plugin README, MCP README and SKILL.md pass the copy lint", () => {
  const units = ["README.md", "../skills/README.md", "../skills/skills/railyard/SKILL.md"].flatMap((f) => markdownUnits(R(f), { where: f }));
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
  const plugin = JSON.parse(R("../skills/.claude-plugin/plugin.json"));
  assert.deepEqual(errors(lintUnits([{ text: plugin.description, surface: "docs" }])), []);
});

test("tool titles, descriptions and messages pass the copy lint", () => {
  const src = R("src/tools.mjs");
  const strings = [...src.matchAll(/"([^"\n]{12,})"/g)].map((m) => m[1]).filter((s) => !/^[\^[]/.test(s));
  const units = [...TOOLS.flatMap((t) => [t.title, t.description]), ...strings].map((text) => ({ text, surface: "docs" }));
  const found = errors(lintUnits(units));
  assert.deepEqual(found, [], formatFindings(found));
});
