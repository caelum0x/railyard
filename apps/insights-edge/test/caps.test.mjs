// Caps at the edge (PY1.3): counted always, stored until the cap, and the message says so.
import { test } from "node:test";
import assert from "node:assert/strict";
import { capDecision, capMessage, nextMonth } from "../src/caps.ts";

test("under the cap everything is stored; at the cap nothing new is stored but everything is counted", () => {
  assert.deepEqual(capDecision(0, 100, 10), { stored: 10, capped: 0 });
  assert.deepEqual(capDecision(95, 100, 10), { stored: 5, capped: 5 });
  assert.deepEqual(capDecision(100, 100, 10), { stored: 0, capped: 10 });
  assert.deepEqual(capDecision(150, 100, 3), { stored: 0, capped: 3 });
  assert.deepEqual(capDecision(0, 0, 2), { stored: 0, capped: 2 });
});

test("the message says exactly what happens at 100%, and warns from 80%", () => {
  const now = new Date("2026-09-29T12:00:00Z");
  assert.equal(nextMonth(now), "2026-10-01");
  assert.equal(nextMonth(new Date("2026-12-31T23:00:00Z")), "2027-01-01");
  assert.equal(capMessage(10, 100, now), null);
  assert.equal(capMessage(80, 100, now), "80 of this month's 100 stored events are used.");
  assert.equal(
    capMessage(3_000_000, 3_000_000, now),
    "This month's 3,000,000 stored events are used. New events are still counted, and are not stored until 2026-10-01. Nothing stored is removed.",
  );
});

test("the page tag browsers load names no upstream product (PY1.3 acceptance 2)", async () => {
  const { TRACKER_JS } = await import("../src/tracker.mjs");
  assert.doesNotMatch(TRACKER_JS, /umami/i);
});
