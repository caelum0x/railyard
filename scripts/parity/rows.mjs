/**
 * Railyard parity rows (Wave Q, QY1.0).
 *
 * Source: PARITY-RECOUNT-2026-09-30.md section 5, on main at ae0f113.
 * Section 5 names only the remaining PARTIAL and MISSING rows. The other ids
 * in I1-I44, A1-A26 and L1-L12 are the regrouped DONE rows (infra 26, analytics
 * PostHog+Mixpanel 19, lim.run 1). This file is the full registry those counts
 * come from. Status is the static record of that recount, not a live re-grade.
 *
 * Mapping notes (the recount's ids, not a second numbering):
 * - Infra is 44 rows I1-I44. The 2026-09-29 matrix had 67 rows; section 5
 *   regrouped them and did not reprint the DONE names. DONE names below follow
 *   that matrix (Relay, Rago, Mint, forge, shared infra) plus the slot adapters
 *   the recount counts as new (compat 30-37).
 * - Analytics is A1-A26 (PostHog + Mixpanel) plus L1-L12 (lim.run G1-G12).
 *   L11 is n/a in the recount and is not counted in 20/9/8. L12 is DONE.
 * - "none" evidence means the recount says nothing exists (MISSING).
 *
 * Evidence is a repo path or a test name taken from section 5, or, for a DONE
 * row the recount does not restate, the path the 2026-09-29 matrix and the
 * landed package used.
 */

/** @typedef {'DONE' | 'PARTIAL' | 'MISSING'} Status */

/**
 * @typedef {Object} Row
 * @property {string} id
 * @property {'infra' | 'analytics' | 'limrun'} group
 * @property {string} name
 * @property {Status} status
 * @property {string} evidence path or test name; "none" when the recount has none
 * @property {string} [note]
 */

/** @type {readonly Row[]} */
export const INFRA = Object.freeze([
  { id: 'I1', group: 'infra', name: 'Relay OpenAI-compatible API, streaming, tools and batch', status: 'DONE', evidence: 'apps/relay/crates/tensorzero-core/src/endpoints/inference.rs' },
  { id: 'I2', group: 'infra', name: 'Provider fallback, spend caps, rate limits and response cache', status: 'DONE', evidence: 'apps/relay/crates/tensorzero-core/src/db/cache.rs' },
  { id: 'I3', group: 'infra', name: 'Price catalog with refresh and per-key usage export', status: 'DONE', evidence: 'apps/relay/crates/relay-catalog/src/openrouter.rs' },
  { id: 'I4', group: 'infra', name: 'Portfolio add-on entitlement', status: 'DONE', evidence: '30-portfolio-addon.test.mjs' },
  { id: 'I5', group: 'infra', name: 'Ship on push with PR previews', status: 'PARTIAL', evidence: 'apps/console/lib/ship', note: 'P-Y10, P-Y11' },
  { id: 'I6', group: 'infra', name: 'Custom domains for shipped apps', status: 'MISSING', evidence: 'none', note: 'no domain code in packages/cli/src' },
  { id: 'I7', group: 'infra', name: 'Speed insights and deploy markers', status: 'PARTIAL', evidence: 'packages/insights-web/src/metrics.ts', note: 'P-Y12' },
  { id: 'I8', group: 'infra', name: 'Deploy slot', status: 'DONE', evidence: '31-deploy.test.mjs' },
  { id: 'I9', group: 'infra', name: 'Analytics slot (Insights)', status: 'DONE', evidence: '32-analytics.test.mjs' },
  { id: 'I10', group: 'infra', name: 'Flag targeting and % rollout', status: 'PARTIAL', evidence: 'apps/product-analytics/src/flags/variants.ts', note: 'plain flags are {key, enabled, value}' },
  { id: 'I11', group: 'infra', name: 'Flags slot', status: 'DONE', evidence: '34-flags.test.mjs' },
  { id: 'I12', group: 'infra', name: 'AI slot (Relay BYOK per project)', status: 'DONE', evidence: '33-ai.test.mjs' },
  { id: 'I13', group: 'infra', name: 'Uptime slot and status page', status: 'DONE', evidence: '35-uptime.test.mjs' },
  { id: 'I14', group: 'infra', name: 'Errors slot (GlitchTip)', status: 'DONE', evidence: 'apps/console/lib/watch' },
  { id: 'I15', group: 'infra', name: 'Consent slot', status: 'DONE', evidence: '37-consent.test.mjs' },
  { id: 'I16', group: 'infra', name: 'Add-on grants and usage', status: 'DONE', evidence: 'apps/rago/seeds/portfolio/grants.json' },
  { id: 'I17', group: 'infra', name: 'Embeddings route', status: 'PARTIAL', evidence: 'apps/relay/config/tensorzero.toml', note: 'embedding model at line 212; no test of our own' },
  { id: 'I18', group: 'infra', name: 'Semantic cache', status: 'MISSING', evidence: 'none' },
  { id: 'I19', group: 'infra', name: 'Guardrails and PII redaction', status: 'MISSING', evidence: 'none' },
  { id: 'I20', group: 'infra', name: 'Maintained engine and OTel export', status: 'PARTIAL', evidence: 'docs/adr/RY-0001-relay-engine.md', note: 'RY-RL1 and RY-RL2 landed; otlp exists but is not configured' },
  { id: 'I21', group: 'infra', name: 'Per-key trace view', status: 'DONE', evidence: 'apps/relay/crates/relay-traces' },
  { id: 'I22', group: 'infra', name: 'Billing slot (seller-own payments)', status: 'PARTIAL', evidence: 'apps/mint/crates/mint-payments/src/seller', note: 'P-X4; CA/adapters/railyard/billing.rs is in the Scribase repo' },
  { id: 'I23', group: 'infra', name: 'Rago event ingest, aggregations and charge models', status: 'DONE', evidence: 'apps/rago/crates/rago-billing/src/aggregation.rs' },
  { id: 'I24', group: 'infra', name: 'Rago usage, entitlements, wallets and portal', status: 'DONE', evidence: 'apps/rago/crates/rago-api/src/lib.rs' },
  { id: 'I25', group: 'infra', name: 'Shadow metering and relay spend into Rago', status: 'DONE', evidence: 'apps/relay/crates/relay-cost-feed' },
  { id: 'I26', group: 'infra', name: 'Portfolio meters (bundle seeds)', status: 'DONE', evidence: 'apps/rago/seeds/portfolio' },
  { id: 'I27', group: 'infra', name: 'Host metrics, exporters and alert rules', status: 'DONE', evidence: 'docs/analytics/CAPACITY.md', note: 'RY-OB1 and RY-OB4 landed in infra-box, not this repo' },
  { id: 'I28', group: 'infra', name: 'Rago load test and per-org ingest limit', status: 'PARTIAL', evidence: 'none', note: 'RY-RG2; recount evidence is none' },
  { id: 'I29', group: 'infra', name: 'Mint card, IAP webhooks, ledger and receipts', status: 'DONE', evidence: 'apps/mint/crates/mint-payments/src/card' },
  { id: 'I30', group: 'infra', name: 'Mint shadow deploy and outbound signed events', status: 'DONE', evidence: 'apps/mint/crates/mint-server/src/shadow.rs' },
  { id: 'I31', group: 'infra', name: 'Direct ASSN v2 and Play RTDN in Mint', status: 'MISSING', evidence: 'none', note: 'RY-MT3' },
  { id: 'I32', group: 'infra', name: 'Stripe Connect onboarding', status: 'PARTIAL', evidence: 'apps/mint/crates/mint-payments/src/seller/stripe.rs', note: 'gated on OD-9' },
  { id: 'I33', group: 'infra', name: 'Forge repos, PRs, reviews and git transport', status: 'DONE', evidence: 'apps/rithub/README.md' },
  { id: 'I34', group: 'infra', name: 'Forge CI checks and runners', status: 'PARTIAL', evidence: 'apps/rithub/crates/rithub-server/src/ci.rs', note: 'no runner (RY-RH1)' },
  { id: 'I35', group: 'infra', name: 'Forge packages, LFS, wikis', status: 'MISSING', evidence: 'none' },
  { id: 'I36', group: 'infra', name: 'Metrics store (VictoriaMetrics)', status: 'DONE', evidence: 'docs/analytics/CAPACITY.md', note: 'IB/stacks/victoriametrics is in infra-box' },
  { id: 'I37', group: 'infra', name: 'Log store (VictoriaLogs)', status: 'PARTIAL', evidence: 'docs/analytics/CAPACITY.md', note: 'IB/stacks/victorialogs; P-Y13 is in infra-box' },
  { id: 'I38', group: 'infra', name: 'Error tracking (GlitchTip) and journey canaries', status: 'DONE', evidence: 'apps/console/app/(app)/watch' },
  { id: 'I39', group: 'infra', name: 'Public status page and nightly backups with box 2 PITR', status: 'DONE', evidence: 'apps/console/app/status' },
  { id: 'I40', group: 'infra', name: 'Restore drills with measured RTO', status: 'PARTIAL', evidence: 'infra-box/backup/restore-drill.sh', note: 'recount names restore-drill.sh; RY-BK2; the script is in infra-box, not this repo' },
  { id: 'I41', group: 'infra', name: 'CI and supply chain', status: 'PARTIAL', evidence: '.github/workflows/ci.yml', note: 'ci, ship, audit (gitleaks, cargo-audit); image scan RY-CI2 open' },
  { id: 'I42', group: 'infra', name: 'Secrets source of truth', status: 'DONE', evidence: '.gitleaks.toml', note: 'RY-SEC1 landed in infra-box' },
  { id: 'I43', group: 'infra', name: 'Edge WAF as code', status: 'DONE', evidence: 'policy/copy-denylist.json', note: 'RY-WAF1 landed in infra-box' },
  { id: 'I44', group: 'infra', name: 'Origin rate limit and bad-IP blocking', status: 'MISSING', evidence: 'none', note: 'RY-WAF2' },
]);

/** @type {readonly Row[]} */
export const ANALYTICS = Object.freeze([
  { id: 'A1', group: 'analytics', name: 'Capture, batch, identify, groups and autocapture', status: 'DONE', evidence: 'apps/product-analytics/test/a1/protocol.test.ts' },
  { id: 'A2', group: 'analytics', name: 'Person merge rules and GDPR delete', status: 'DONE', evidence: 'apps/product-analytics/test/a1/identity.test.ts' },
  { id: 'A3', group: 'analytics', name: 'React Native and Node SDK clients', status: 'DONE', evidence: 'apps/product-analytics/test/a1/sdk-node.test.ts' },
  { id: 'A4', group: 'analytics', name: 'Native iOS, Android, Flutter SDKs', status: 'PARTIAL', evidence: 'apps/product-analytics/src/capture', note: 'service speaks the PostHog protocol; no ios, android or flutter wrapper' },
  { id: 'A5', group: 'analytics', name: 'Trends', status: 'DONE', evidence: 'apps/product-analytics/test/a2/trends.test.ts' },
  { id: 'A6', group: 'analytics', name: 'Funnels', status: 'DONE', evidence: 'apps/product-analytics/test/a2/funnels.test.ts' },
  { id: 'A7', group: 'analytics', name: 'Retention', status: 'DONE', evidence: 'apps/product-analytics/test/a2/grids.test.ts' },
  { id: 'A8', group: 'analytics', name: 'Paths', status: 'DONE', evidence: 'apps/product-analytics/test/a2/grids.test.ts' },
  { id: 'A9', group: 'analytics', name: 'Stickiness and lifecycle', status: 'DONE', evidence: 'apps/product-analytics/test/a2/grids.test.ts' },
  { id: 'A10', group: 'analytics', name: 'Actions', status: 'DONE', evidence: 'apps/product-analytics/test/a2/actions-cohorts.test.ts' },
  { id: 'A11', group: 'analytics', name: 'Cohorts', status: 'DONE', evidence: 'apps/product-analytics/test/a2/actions-cohorts.test.ts' },
  { id: 'A12', group: 'analytics', name: 'Dashboards and saved insights', status: 'DONE', evidence: 'apps/product-analytics/test/a2/dashboards.test.ts' },
  { id: 'A13', group: 'analytics', name: 'SQL over events', status: 'DONE', evidence: 'apps/product-analytics/test/a2/sandbox.test.ts' },
  { id: 'A14', group: 'analytics', name: 'Session replay', status: 'DONE', evidence: 'apps/product-analytics/test/a3/replay.test.ts' },
  { id: 'A15', group: 'analytics', name: 'Heatmaps', status: 'DONE', evidence: 'apps/product-analytics/test/a3/heatmaps.test.ts' },
  { id: 'A16', group: 'analytics', name: 'Surveys', status: 'DONE', evidence: 'apps/product-analytics/test/a3/surveys.test.ts' },
  { id: 'A17', group: 'analytics', name: 'Experiments', status: 'DONE', evidence: 'apps/product-analytics/test/a3/experiments.test.ts' },
  { id: 'A18', group: 'analytics', name: 'Web analytics', status: 'PARTIAL', evidence: 'apps/insights', note: 'Insights live on loopback; P-Y12; no web-analytics view in product analytics' },
  { id: 'A19', group: 'analytics', name: 'Feature flags at PostHog depth', status: 'PARTIAL', evidence: 'apps/product-analytics/src/flags/routes.ts', note: 'targeting rules and plain % rollout open (I10)' },
  { id: 'A20', group: 'analytics', name: 'Remote config', status: 'DONE', evidence: 'apps/product-analytics/test/a1/protocol.test.ts' },
  { id: 'A21', group: 'analytics', name: 'Group analytics', status: 'DONE', evidence: 'apps/product-analytics/src/groups' },
  { id: 'A22', group: 'analytics', name: 'Data export and warehouse sync', status: 'PARTIAL', evidence: 'apps/product-analytics/test/a3/exports.test.ts', note: 'S3 NDJSON.gz and CSV; no warehouse destinations' },
  { id: 'A23', group: 'analytics', name: 'CDP destinations', status: 'PARTIAL', evidence: 'apps/product-analytics/src/exports/destinations.ts', note: 'signed webhook with retry; no catalogue or transforms' },
  { id: 'A24', group: 'analytics', name: 'Retention limits and plan caps', status: 'DONE', evidence: 'apps/product-analytics/test/a1/retention.test.ts' },
  { id: 'A25', group: 'analytics', name: 'Alerts and subscriptions', status: 'MISSING', evidence: 'none', note: 'nothing in apps/product-analytics/src' },
  { id: 'A26', group: 'analytics', name: 'scribase add analytics reaches product analytics', status: 'PARTIAL', evidence: 'apps/console/app/api/v1/bridge/scribase/service.ts', note: 'no product_analytics case; SB/config/capabilities/analytics.json is Insights-only' },
]);

/**
 * Limrun gaps G1-G12. L11 is n/a and is excluded from the 20/9/8 analytics total.
 * @type {readonly Row[]}
 */
export const LIMRUN = Object.freeze([
  { id: 'L1', group: 'limrun', name: 'Per-resource MCP URL (G1)', status: 'MISSING', evidence: 'none' },
  { id: 'L2', group: 'limrun', name: 'Signed share links (G2)', status: 'PARTIAL', evidence: 'apps/product-analytics/src/replay/shared/signed-link.ts', note: 'no shared contract; no branch or deploy links' },
  { id: 'L3', group: 'limrun', name: 'Labels and reuse-if-exists (G3)', status: 'MISSING', evidence: 'none' },
  { id: 'L4', group: 'limrun', name: 'TTL and idle timeout (G4)', status: 'PARTIAL', evidence: 'SB/crates/policy/src/preview.rs', note: 'recount: previews ttl_hours; no idle timeout; P-S18. The file is in the Scribase repo, not this one' },
  { id: 'L5', group: 'limrun', name: 'Jurisdiction placement (G5)', status: 'MISSING', evidence: 'none', note: 'owner decision' },
  { id: 'L6', group: 'limrun', name: 'Python, Go and Java SDKs (G6)', status: 'PARTIAL', evidence: 'apps/product-analytics/test/a1/sdk-node.test.ts', note: 'upstream-compatible clients; no compat result published' },
  { id: 'L7', group: 'limrun', name: '/openapi.json (G7)', status: 'MISSING', evidence: 'none' },
  { id: 'L8', group: 'limrun', name: 'Embeddable replay (G8)', status: 'MISSING', evidence: 'none' },
  { id: 'L9', group: 'limrun', name: 'Cloud simulators (G9)', status: 'MISSING', evidence: 'none', note: 'belongs to Recoder' },
  { id: 'L10', group: 'limrun', name: 'Build assets with scoped tokens (G10)', status: 'MISSING', evidence: 'none' },
  { id: 'L11', group: 'limrun', name: 'Idle and build-time billing units (G11)', status: 'DONE', evidence: 'docs/analytics/LIMRUN-GAPS.md', note: 'n/a in the recount; not counted in 20/9/8' },
  { id: 'L12', group: 'limrun', name: 'logs.tail MCP tool (G12)', status: 'DONE', evidence: 'docs/analytics/LIMRUN-GAPS.md', note: 'DONE in SB/crates/mcp/src/control.rs:908 (a01da6f); that file is not in this repo' },
]);

/** Every row the recount prints, in id order within each group. */
export const ROWS = Object.freeze([...INFRA, ...ANALYTICS, ...LIMRUN]);

/**
 * Totals the dispatch asks QY1.0 to reproduce on today's main.
 * Infra is I1-I44. Analytics is A1-A26 plus countable lim.run rows (L11 excluded).
 */
export const BASELINE = Object.freeze({
  infra: Object.freeze({ DONE: 26, PARTIAL: 12, MISSING: 6 }),
  analytics: Object.freeze({ DONE: 20, PARTIAL: 9, MISSING: 8 }),
});
