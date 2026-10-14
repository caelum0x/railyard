// Regression guard: the vendored rrweb-player UMD bundle (2.1.7) exposes the
// player class as window.rrwebPlayer.Player (a plain `new window.rrwebPlayer(...)`
// throws "is not a constructor" — found only by actually running the viewer in a
// real browser against the real vendored bundle, not by the Miniflare test suite).
import assert from "node:assert/strict";
import { test } from "node:test";
import { REPLAY_JS, REPLAY_VIEWER_JS } from "../../src/replay/tracker.mjs";

test("the player viewer resolves window.rrwebPlayer.Player (or .default), never calls `new window.rrwebPlayer` directly", () => {
  assert.match(REPLAY_VIEWER_JS, /rrwebPlayer\.Player\|\|window\.rrwebPlayer\.default/);
  assert.doesNotMatch(REPLAY_VIEWER_JS, /new window\.rrwebPlayer\(/);
});

test("the recorder loader checks Do Not Track and Global Privacy Control before any fetch", () => {
  const dntCheckIndex = REPLAY_JS.indexOf("doNotTrack");
  const firstFetchIndex = REPLAY_JS.indexOf("fetch(");
  assert.ok(dntCheckIndex >= 0 && dntCheckIndex < firstFetchIndex, "the DNT/GPC check must run before the first network call");
  assert.match(REPLAY_JS, /globalPrivacyControl/);
});
