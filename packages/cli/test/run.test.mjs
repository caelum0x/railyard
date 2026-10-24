// run()'s `capture` option: a long-running stream (railyard logs wraps `wrangler
// tail`, which can run for hours) must never buffer the text it prints into
// memory just because something eventually checks the exit code.
import { test } from "node:test";
import assert from "node:assert/strict";

import { run } from "../src/run.mjs";

const LINES = 20_000;
const printLines = `for (let i = 0; i < ${LINES}; i++) { process.stdout.write("line " + i + "\\n"); }`;

test("capture: true (the default) still returns the full output, as deploy/rollback need", async () => {
  const seen = [];
  const r = await run(process.execPath, ["-e", printLines], { env: process.env, echo: (s) => seen.push(s) });
  assert.equal(r.code, 0);
  assert.equal(r.stdout.split("\n").filter(Boolean).length, LINES);
  assert.ok(seen.length > 0, "echo still fires with capture on");
});

test("capture: false streams the same output through `echo` but never buffers it", async () => {
  const seen = [];
  const r = await run(process.execPath, ["-e", printLines], { env: process.env, echo: (s) => seen.push(s), capture: false });
  assert.equal(r.code, 0);
  // The whole point: unlike the capture:true run above, stdout/stderr stay empty
  // no matter how much the child prints, so a day-long `railyard logs` tail
  // never grows these strings.
  assert.equal(r.stdout, "");
  assert.equal(r.stderr, "");
  // But every line still reached the caller live -- nothing was silently dropped.
  assert.equal(seen.join("").split("\n").filter(Boolean).length, LINES);
});

test("capture: false still reports a spawn error (node:child_process ENOENT), just without buffered text", async () => {
  const r = await run("railyard-definitely-not-a-real-binary", [], { env: process.env, capture: false });
  assert.equal(r.code, 127);
  assert.match(r.stderr, /ENOENT/);
});
