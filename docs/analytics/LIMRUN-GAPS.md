# lim.run (Limrun) vs Scribase: gap audit (2026-09-30)

- Read only. Nothing in Scribase was edited.
- Sources:
  - `https://lim.run` (301 to `https://limrun.com/`), `https://docs.limrun.com/docs`, `/docs/concepts` and `/docs/agents/remote-mcp`, all fetched 2026-09-30;
  - `gh api orgs/limrun-inc/repos` (2026-09-30);
  - the earlier research in `gtm/VIBE-CODER-PLATFORM-PLAN-2026-09-28.md` section 1.2.
- Scribase facts come from the repo at `supabase-plus-clone/upstream/supabase/supabase/scribase` (`SB/`), `main` at `4e49f65`.
- This is an internal planning note. It is not public copy; public pages never name the reference product.

## 1. What Limrun is

Limrun sells cloud mobile sandboxes for coding agents and CI. Its instance types are:
- iOS simulators;
- Android emulators;
- Xcode sandboxes (remote build and signing on a cloud Mac);
- Gradle sandboxes.

Around them it offers:
- asset storage for builds (IPA, APK, AAB, simulator builds, encrypted keychain snapshots);
- a browser live view (`<RemoteControl />` in `@limrun/ui`);
- 60 fps video, accessibility-tree reads, tap and type, and app logs;
- Maestro, Appium, XCTest and Detox support;
- a GitHub Action that drops iOS preview links on PRs (`limrun-inc/ios-preview-action`).

Access and developer experience:
- **Clients:**
  - a `lim` CLI (npm and Homebrew);
  - SDKs in TypeScript, Python, Go and Java (Apache-2.0 on GitHub);
  - a REST API with `/openapi.json`;
  - an organization-wide remote MCP server (`mcp.limrun.com/mcp`, 16 tools, OAuth by default or a bearer API key for CI);
  - **a separate MCP server per instance**, authenticated by that instance's own token;
  - an agent-skills catalog (`limrun-inc/skills`, MIT).
- **Instances:**
  - lifecycle states: creating, assigned, ready, terminated;
  - an inactivity timeout and a hard timeout;
  - labels with `--reuse-if-exists`, and bulk cleanup by label selector;
  - placement by jurisdiction (US, EU or Asia, a hard constraint), by hints, or by the deprecated region field.
- **Credentials:** five kinds.
  - organization API keys;
  - instance tokens;
  - stream-only tokens;
  - **signed stream URLs** a human can open without signing in;
  - scoped time-limited registry tokens.
- **Pricing:** billed on idle time and build time. There is no public rate card and no free tier was found.

## 2. Where Scribase already matches

| Limrun pattern | Scribase today (file) |
|---|---|
| CLI with device-flow login | `SB/crates/scribase-cli/src/device_login.rs`, `SB/packages/scribase-js/src/cli/login.ts` |
| Organization-wide remote MCP with OAuth and scoped tokens | `SB/crates/mcp/src/oauth/**`, `SB/crates/mcp/src/scope.rs` (scopes, destructive guard, production guard) |
| Agent skills and rules as a package | `SB/agents/**` (plugin, skills, rules for Claude Code, Codex, Cursor and Windsurf; MCP Registry entry) |
| PR preview with a status comment | `SB/crates/control-api/src/http/previews.rs`, `adapters/branch_pr.rs` (GitHub and GitLab status comment through the forge webhook) |
| Ephemeral, claimable resources | `SB/crates/control-api/src/http/temporary.rs` (proof-of-work temporary projects with a claim link) |
| REST API plus typed SDK | `SB/packages/scribase-js/src/resources/**`, `SB/crates/sdk` |
| Idempotent create calls | `SB/packages/scribase-js/src/idempotency.ts` |

## 3. Gaps (what Scribase lacks versus Limrun), with a suggested owner lane

| # | Gap | Why it matters for Scribase customers | Suggested lane and shape | Size |
|---|---|---|---|---|
| G1 | **Per-resource MCP endpoint.** Limrun gives each instance its own MCP URL whose token can only touch that instance. Scribase has one MCP URL; scoping is per token, not per URL. | An agent working on one branch cannot touch another project, even with a leaked token | **PS lane** (MCP is PS-owned outside the carve-out): `https://api.scribase.com/mcp/p/<project>/<branch>` with a branch-bound, expiring token from `scope.rs`; `SB/crates/mcp/src/dev/railyard/**` (PY3.4) exposes the slot tools under the same URL | M |
| G2 | **Signed no-login share links.** Limrun hands humans a signed stream URL. Scribase has no signed share link for a branch's data view, a replay, an insight or a deploy preview. | Agents post "here is the result" into a PR or Slack, and a human opens it without an account | **PS lane** for branch views; **PY lane** for Railyard: product-analytics A3 already signs 10-minute replay and heatmap URLs; a shared "share link" contract (HMAC, `exp`, single resource) lets both use one format | S-M |
| G3 | **Labels and reuse-if-exists on create.** Limrun's create returns an existing instance whose labels match, and cleans up by label selector. Scribase branches and temporary projects have names, not labels, and no bulk cleanup. | Agents re-running a task create duplicate branches; CI leaks previews | **PS lane**: `labels jsonb` on environments, `reuse_if_exists` on create, `DELETE ...?selector=` guarded by the destructive scope | S |
| G4 | **Inactivity and hard timeouts per resource.** Limrun has both on every instance. Scribase has sleep state (`sleep_state.rs`) and a 72-hour temporary-project expiry, but no per-branch hard TTL set by the caller. | Agent branches should end on their own | **PS lane**: `ttl` and `idle_timeout` on branch create, enforced by the existing lifecycle beat | S |
| G5 | **Jurisdiction as a hard placement constraint** (US, EU, Asia). Scribase takes a `region` on project create (`organizations.rs`) but has no jurisdiction guarantee or EU-only promise. | EU customers need data residency, and analytics data raises the same question | **PS lane** for projects; **PY lane** notes that product analytics lives on box 2 (one location) and says so in its privacy page until a second region exists. Owner decision: which regions exist | M, owner-gated |
| G6 | **SDKs in Python, Go and Java.** Limrun ships four official SDKs. Scribase ships JS and Rust; Python and Go users rely on the upstream-compatible clients without a published compatibility result. | Backend developers and CI scripts outside JS | **PS lane**: run the upstream-compatible Python and Go clients against the compat suite and publish the result; generate thin management SDKs from the control-api OpenAPI only if the compat run shows gaps | M |
| G7 | **OpenAPI document published at a stable URL.** Limrun serves `/openapi.json`. | Agents and code generators discover the API | **PS lane**: serve the control-api OpenAPI at `/openapi.json` and add it to `SB/agents/**` | S |
| G8 | **Embeddable live view component** (`<RemoteControl />`). Scribase has no embeddable React component for its own resources. | A customer could embed "branch data" or "replay" in their own tools | **PY lane**: `@railyard/analytics-js` could later export a replay embed built on the A3 player page (signed URL). **PS lane**: not needed now | S |
| G9 | **Cloud simulators, emulators and remote Xcode/Gradle builds.** This is Limrun's core product. Scribase has none. | Mobile app developers on Scribase need to see their Expo app run from an agent | **Not Scribase.** Per `VIBE-CODER-PLATFORM-PLAN` VC12, this belongs to **Recoder** (mobile-lane gate step with Maestro, Apache-2.0, on our own Mac, with an optional paid-provider adapter). Scribase should only offer the hook: a branch preview record that can hold a video and build artifact link | L, owner decision |
| G10 | **Build asset storage with scoped registry tokens.** | Mobile build artifacts next to the backend | **PS lane** (Storage already exists): a `builds` bucket convention plus time-limited scoped read tokens reusing `storage_s3_keys.rs`; only after G9 is decided | S |
| G11 | **Idle and build-time billing units.** Limrun bills on running time only. Scribase has plans plus Rago usage meters. | Not a gap to copy for databases; noted for the Recoder mobile lane if built | none now | n/a |
| G12 | **Per-instance log streaming to the agent** (`ios-logs`). Scribase has logs (`scribase.app_logs`, VictoriaLogs in PS3.5) but no MCP tool that tails a branch's function and database logs live. | Agents debug without a console | **PS lane**: `logs.tail` MCP tool over the PS3.5 log store, scoped by G1's URL | S |

## 4. Suggested order (owner can reorder)

1. **G1 + G12 + G7** (PS lane): the agent surface. Small, and it makes Scribase the backend that agents can operate safely.
2. **G2** (PS for branch views + PY for analytics links, one shared signing contract), then **G3** and **G4** (PS).
3. **G6** (PS: compat run first, SDK generation only for real gaps).
4. **G5** and **G9** need owner decisions:
   - G5: which regions and jurisdictions exist.
   - G9: whether the cloud mobile lane is built in Recoder, and whether on our own Mac or through a paid provider.

## 5. What product analytics (this branch) contributes

- A3's signed, expiring replay and heatmap links follow the G2 shape (HMAC over `{resource, project, exp}`), so the PS lane can adopt one format.
- A1's service exposes its proof and summary through the existing bridge. This lets PY3.4's MCP tools (`analytics.summary`) sit behind G1's per-project URL with no extra work.
