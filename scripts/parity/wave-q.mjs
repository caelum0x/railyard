/**
 * Wave Q verified row changes (QY verify, 2026-10-05).
 *
 * rows.mjs stays the static record of PARITY-RECOUNT-2026-09-30.md section 5
 * (the 26/12/6 and 20/9/8 baseline). Each entry here moves one row on the
 * strength of a test file that the QY verifier ran green on wave-q-railyard
 * before the merge to main. The recount refuses an entry whose evidence file
 * is missing, so a deleted test drops the row back to its baseline status.
 */

/** @typedef {import('./rows.mjs').Status} Status */

/**
 * @typedef {Object} Change
 * @property {string} id
 * @property {Status} status
 * @property {string} evidence repo path of the test that proves the row
 * @property {string} pkg the Wave Q package
 * @property {string} [note]
 */

/** @type {readonly Change[]} */
export const WAVE_Q = Object.freeze([
  { id: 'I5', status: 'DONE', pkg: 'QY1.1', evidence: 'apps/console/lib/ship/ship.test.ts', note: 'P-Y10 session-bound link/go and Cloudflare step; P-Y11 finish by stored repo_id' },
  { id: 'I6', status: 'PARTIAL', pkg: 'QY1.5', evidence: 'packages/cli/test/domains.test.mjs', note: 'CLI, console and Scribase Deploy tab tested against a mocked Cloudflare API; the owner-run proof on a real domain is still open' },
  { id: 'I7', status: 'DONE', pkg: 'QY1.2', evidence: 'apps/insights-edge/test/vitals.test.mjs', note: '/s.js sends LCP, INP and CLS (P-Y12)' },
  { id: 'I10', status: 'DONE', pkg: 'QY1.3', evidence: 'packages/openfeature-railyard/test/targeting.test.mjs', note: 'property, cohort and segment rules; stable-hash % rollout; local and /flags agree' },
  { id: 'A18', status: 'DONE', pkg: 'QY1.2', evidence: 'apps/product-analytics/test/qy/web.test.ts', note: 'pageviews, top pages, referrers, UTM and vitals p75 from the product-analytics store' },
  { id: 'A19', status: 'DONE', pkg: 'QY1.3', evidence: 'apps/product-analytics/test/qy/flags-targeting.test.ts', note: 'targeting rules and % rollout (I10)' },
  { id: 'A23', status: 'PARTIAL', pkg: 'QY1.4', evidence: 'apps/product-analytics/test/qy/destinations.test.ts', note: 'Slack, Discord, HubSpot and webhook catalogue with signing, retry and log; no transforms' },
  { id: 'A25', status: 'DONE', pkg: 'QY1.4', evidence: 'apps/product-analytics/test/qy/alerts.test.ts', note: 'threshold and anomaly alerts, weekly subscriptions, unsubscribe' },
  { id: 'A26', status: 'DONE', pkg: 'QY1.2', evidence: 'apps/product-analytics/test/qy/first-product-event.test.ts', note: 'bridge product_analytics case; first product event proof' },
]);
