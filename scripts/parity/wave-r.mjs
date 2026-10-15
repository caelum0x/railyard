/**
 * Wave R verified row changes (parity recount refresh, 2026-10-15).
 *
 * rows.mjs and wave-q.mjs stop at the 2026-10-05/10-06 record. Code merged to
 * main after that date (A4 native SDKs 2026-10-06, A22 warehouse destinations
 * and A23 transforms/catalogue 2026-10-06) was never folded back into the
 * recount, so the static table kept calling them PARTIAL after they shipped.
 * This wave closes that gap on the evidence actually in the repo today, not
 * on the plan file's own claims.
 *
 * Verified 2026-10-15:
 * - `pnpm run test:all` in apps/product-analytics: 216 tests, 212 pass, 0
 *   fail, 4 skipped (no cached Chromium locally; unrelated to A4/A22/A23).
 *   That run includes test/qy/warehouse.test.ts, test/qy/snowflake.test.ts,
 *   test/qy/transforms.test.ts and test/qy/destinations.test.ts, all green.
 * - `swift test` in packages/analytics-swift: 6/6 tests pass.
 * - packages/analytics-kotlin and packages/analytics-flutter have their own
 *   substantive test suites (not stubs) but were not re-run in this
 *   environment (no kotlinc or flutter toolchain here); see their own
 *   READMEs for how to run them.
 *
 * Like Wave Q, a change here is applied only when its evidence file exists in
 * the repo (recount.mjs:applyChanges), and a later wave wins over an earlier
 * one on the same row id.
 */

/** @typedef {import('./rows.mjs').Status} Status */

/**
 * @typedef {Object} Change
 * @property {string} id
 * @property {Status} status
 * @property {string} evidence repo path of the test that proves the row
 * @property {string} pkg the wave/date tag
 * @property {string} [note]
 */

/** @type {readonly Change[]} */
export const WAVE_R = Object.freeze([
  {
    id: 'A4',
    status: 'DONE',
    pkg: '2026-10-15',
    evidence: 'packages/analytics-swift/Tests/RailyardAnalyticsTests/AnalyticsTests.swift',
    note:
      'native Swift (iOS/macOS/tvOS/watchOS), Kotlin (JVM/Android) and Flutter/Dart wrapper SDKs landed 2026-10-06 (fba4dba7, 8cd06952); each has its own unit test suite. Swift suite verified green here (swift test, 6/6); Kotlin and Flutter suites were not re-run in this environment (no kotlinc or flutter toolchain) but their test files are real, not stubs. No sample/example app end-to-end proof yet.',
  },
  {
    id: 'A22',
    status: 'DONE',
    pkg: '2026-10-15',
    evidence: 'apps/product-analytics/test/qy/warehouse.test.ts',
    note:
      'ClickHouse and BigQuery (0e611a19) and Snowflake (aa25b536) destinations next to the S3/webhook export, all landed 2026-10-06. test/qy/warehouse.test.ts and test/qy/snowflake.test.ts both green in the 2026-10-15 full run.',
  },
  {
    id: 'A23',
    status: 'DONE',
    pkg: '2026-10-15',
    evidence: 'apps/product-analytics/test/qy/transforms.test.ts',
    note:
      'Slack, Discord and HubSpot catalogue destinations (b4612a4f) plus where/keep-drop/rename/set transforms (41653aeb), all landed 2026-10-05/06. test/qy/destinations.test.ts and test/qy/transforms.test.ts both green in the 2026-10-15 full run.',
  },
]);
