// The console runs without the Dodo brand: billing is off, everything else on.

import { test } from "node:test";
import assert from "node:assert/strict";

import { readConfig } from "../lib/config.ts";

const BASE = {
  SCRIBASE_URL: "https://ry.scribase.com/",
  SCRIBASE_ANON_KEY: "anon",
  SCRIBASE_SERVICE_ROLE_KEY: "service",
  RAILYARD_SESSION_SECRET: "s".repeat(40),
};
const DODO = {
  DODO_API_KEY: "test_key",
  DODO_WEBHOOK_SECRET: "whsec_x",
  DODO_PRODUCT_ID_CARD_CHECK: "pdt_card",
  DODO_PRODUCT_ID_MONTH: "pdt_month",
  DODO_PRODUCT_ID_YEAR: "pdt_year",
};

test("without the Dodo keys the console still starts, with billing off", () => {
  const cfg = readConfig(BASE);
  assert.equal(cfg.billingReady, false);
  assert.equal(cfg.scribaseUrl, "https://ry.scribase.com");
  assert.equal(cfg.dodoApiKey, "");
});

test("billing is on only when the key, the webhook secret and all three products are set", () => {
  assert.equal(readConfig({ ...BASE, ...DODO }).billingReady, true);
  for (const name of Object.keys(DODO)) {
    const env: Record<string, string> = { ...BASE, ...DODO, [name]: " " };
    assert.equal(readConfig(env).billingReady, false, `${name} missing`);
  }
});

test("the Scribase project and the session secret are still required", () => {
  for (const name of Object.keys(BASE)) {
    const env: Record<string, string> = { ...BASE, ...DODO };
    delete env[name];
    assert.throws(() => readConfig(env), new RegExp(name));
  }
});

test("the live payment host still needs the owner's switch", () => {
  assert.throws(() => readConfig({ ...BASE, ...DODO, DODO_API_BASE: "https://live.dodopayments.com" }), /RAILYARD_DODO_LIVE/);
});

test("the live payment host rejects any RAILYARD_DODO_LIVE value except exactly \"1\"", () => {
  for (const value of ["true", "yes", "on", "01", " 1", "1 ", ""]) {
    assert.throws(
      () => readConfig({ ...BASE, ...DODO, DODO_API_BASE: "https://live.dodopayments.com", RAILYARD_DODO_LIVE: value }),
      /RAILYARD_DODO_LIVE/,
      `RAILYARD_DODO_LIVE=${JSON.stringify(value)} must not unlock the live host`,
    );
  }
});

// Found live in production 2026-10-17: the deployed Worker's SCRIBASE_URL var
// was "https://api.scribase.com" -- the shared control-plane management API,
// not a project's own gateway host -- so POST /auth/v1/otp 404'd there
// (route_not_found) and every sign-in silently failed. No Scribase project
// existed for the console at all at the time (checked directly against
// scribase.projects on the box: zero rows for organization_id='ereboslab').
// This guard makes that exact misconfiguration fail loudly at config-read
// time instead of shipping a console that renders but can never sign anyone in.
test("SCRIBASE_URL must be a project's own gateway host, not the control-plane API", () => {
  for (const value of ["https://api.scribase.com", "https://api.scribase.com/", " https://api.scribase.com "]) {
    assert.throws(
      () => readConfig({ ...BASE, ...DODO, SCRIBASE_URL: value }),
      /SCRIBASE_URL/,
      `SCRIBASE_URL=${JSON.stringify(value)} must be rejected as the control-plane host`,
    );
  }
  // A real project host (https://<host_label>.scribase.com, per the box's
  // SCRIBASE_GATEWAY_BASE_URL_TEMPLATE) is unaffected.
  assert.equal(readConfig({ ...BASE, ...DODO, SCRIBASE_URL: "https://railyard-console.scribase.com" }).scribaseUrl, "https://railyard-console.scribase.com");
});
