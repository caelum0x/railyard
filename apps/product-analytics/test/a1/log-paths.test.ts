// Logged request paths never carry a person's id (often an email address):
// the segment after /persons/ is replaced before a path reaches a log line.
import { test } from "node:test";
import assert from "node:assert/strict";
import { logPath } from "../../src/log.ts";

test("logged paths hide person ids and keep the route shape", () => {
  const p = "0b5c3a8e-3f4b-4c7e-9a51-1d2e3f4a5b6c";
  assert.equal(logPath(`http://x/v1/projects/${p}/persons/alice%40x.example/export`), `/v1/projects/${p}/persons/:distinct_id/export`);
  assert.equal(logPath(`http://x/v1/projects/${p}/persons/alice@x.example`), `/v1/projects/${p}/persons/:distinct_id`);
  assert.equal(logPath(`http://x/v1/projects/${p}/persons?search=bob@x.example`), `/v1/projects/${p}/persons`);
  assert.equal(logPath("http://x/batch/"), "/batch/");
});
