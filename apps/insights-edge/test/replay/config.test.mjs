// Pure config tests: privacy-first defaults (off, masked, sampled, capped),
// clamping of out-of-range input, and the settings-form parser.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  DEFAULT_MASK_TEXT_SELECTOR,
  DEFAULT_MAX_BYTES,
  DEFAULT_MAX_SESSION_MS,
  DEFAULT_SAMPLE_RATE,
  DEFAULT_SESSIONS_PER_MONTH,
  MAX_SAMPLE_RATE,
  MIN_SAMPLE_RATE,
  parseReplaySettings,
  publicReplayConfig,
  resolveReplayConfig,
  sessionRoom,
} from "../../src/replay/config.mjs";

test("resolveReplayConfig defaults to off, masked, sampled and capped when a site has no replay object at all", () => {
  const cfg = resolveReplayConfig({ label: "shop.example" });
  assert.equal(cfg.enabled, false);
  assert.equal(cfg.sample_rate, DEFAULT_SAMPLE_RATE);
  assert.equal(cfg.mask_all_inputs, true);
  assert.equal(cfg.mask_text_selector, DEFAULT_MASK_TEXT_SELECTOR);
  assert.equal(cfg.max_session_ms, DEFAULT_MAX_SESSION_MS);
  assert.equal(cfg.max_bytes, DEFAULT_MAX_BYTES);
  assert.equal(cfg.sessions_per_month, DEFAULT_SESSIONS_PER_MONTH);
});

test("resolveReplayConfig clamps out-of-range or malformed stored values instead of trusting them", () => {
  const cfg = resolveReplayConfig({
    replay: { enabled: true, sample_rate: 5, mask_all_inputs: "nope", mask_text_selector: 123, max_session_ms: -10, max_bytes: "lots", sessions_per_month: 0 },
  });
  assert.equal(cfg.enabled, true);
  assert.equal(cfg.sample_rate, MAX_SAMPLE_RATE);
  // mask_all_inputs only turns off with the literal `false`; any other stored value stays masked (fail safe).
  assert.equal(cfg.mask_all_inputs, true);
  assert.equal(cfg.mask_text_selector, DEFAULT_MASK_TEXT_SELECTOR);
  assert.ok(cfg.max_session_ms >= 60_000);
  assert.equal(cfg.max_bytes, DEFAULT_MAX_BYTES);
  assert.ok(cfg.sessions_per_month >= 10);
});

test("resolveReplayConfig honors an explicit mask_all_inputs: false and a narrower mask selector", () => {
  const cfg = resolveReplayConfig({ replay: { mask_all_inputs: false, mask_text_selector: ".secret" } });
  assert.equal(cfg.mask_all_inputs, false);
  assert.equal(cfg.mask_text_selector, ".secret");
});

test("publicReplayConfig exposes only recording knobs, never session data or sessions_per_month", () => {
  const pub = publicReplayConfig({ replay: { enabled: true, sample_rate: 0.5 } });
  assert.deepEqual(Object.keys(pub).sort(), ["enabled", "mask_all_inputs", "mask_text_selector", "max_bytes", "max_session_ms", "sample_rate"].sort());
  assert.equal(pub.sample_rate, 0.5);
});

test("sessionRoom never goes negative", () => {
  assert.equal(sessionRoom(0, 500), 500);
  assert.equal(sessionRoom(500, 500), 0);
  assert.equal(sessionRoom(600, 500), 0);
});

test("parseReplaySettings reads a form-encoded submission (checkbox semantics: present+on means true, absent means false)", () => {
  const on = parseReplaySettings({ enabled: "on", sample_rate: "0.25", mask_all_inputs: "on", mask_text_selector: "" });
  assert.deepEqual(on, { ok: true, replay: { enabled: true, sample_rate: 0.25, mask_all_inputs: true, mask_text_selector: DEFAULT_MASK_TEXT_SELECTOR } });

  const off = parseReplaySettings({});
  assert.equal(off.ok, true);
  assert.equal(off.replay.enabled, false);
  assert.equal(off.replay.mask_all_inputs, true, "an absent checkbox for masking fails safe to masked, not unmasked");

  const unmasked = parseReplaySettings({ mask_all_inputs: "off" });
  assert.equal(unmasked.replay.mask_all_inputs, false);

  const clamped = parseReplaySettings({ sample_rate: "99" });
  assert.equal(clamped.replay.sample_rate, MAX_SAMPLE_RATE);
  const clampedLow = parseReplaySettings({ sample_rate: "-1" });
  assert.equal(clampedLow.replay.sample_rate, MIN_SAMPLE_RATE);
});

test("parseReplaySettings refuses a too-long mask selector", () => {
  const r = parseReplaySettings({ mask_text_selector: "x".repeat(500) });
  assert.equal(r.ok, false);
  assert.match(r.error, /too long/);
});
