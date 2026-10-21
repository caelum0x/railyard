/**
 * Wave S verified row change (Mint Play Store direct integration, 2026-10-21).
 *
 * Closes I31 ("Direct ASSN v2 and Play RTDN in Mint", RY-MT3). Apple's half
 * (App Store Server Notifications v2 + Server API) was already DONE before
 * this wave, verified in apps/mint/crates/mint-payments/src/iap/appstore.rs —
 * this wave adds the other half, Google Play Real-time Developer
 * Notifications + the Play Developer API, as a new direct (no-aggregator)
 * provider: apps/mint/crates/mint-payments/src/iap/playstore.rs.
 *
 * Verified 2026-10-21: `cargo test -p mint-payments -p mint-server` —
 * 112 + 13 + 6 + 6 + 3 (mint-payments) and 22 + 13 + 14 + 3 + 3 + 6 + 8
 * (mint-server) all green, including 18 new playstore.rs unit tests covering
 * token auth, notification-type gating, the Developer API re-confirmation
 * path (OAuth2 service-account JWT over a real RSA test key, not mocked),
 * the per-charge order-id idempotency (a renewal must mint again, not replay
 * the original purchase), account-binding and stale-order-id refusal, and
 * the voided-purchase reversal path.
 */

/** @typedef {import('./rows.mjs').Status} Status */

/**
 * @typedef {Object} Change
 * @property {string} id
 * @property {Status} status
 * @property {string} evidence repo path of the code that proves the row
 * @property {string} pkg the wave/date tag
 * @property {string} [note]
 */

/** @type {readonly Change[]} */
export const WAVE_S = Object.freeze([
  {
    id: 'I31',
    status: 'DONE',
    pkg: '2026-10-21',
    evidence: 'apps/mint/crates/mint-payments/src/iap/playstore.rs',
    note:
      'Apple ASSN v2 + Server API direct (appstore.rs) was already DONE; this wave adds Google Play RTDN + Developer API direct (playstore.rs) — 18 new unit tests, 0 regressions across mint-payments/mint-server. Not yet deployed to railyard-box (code-only; needs a real Play service account + PLAYSTORE_* secrets, an owner step).',
  },
]);
