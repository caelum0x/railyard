# rithub — Enterprise Architecture

> **Scope of this document.** The *target* enterprise shape for `rithub`: the
> self-hostable collaboration forge (the GitHub equivalent) built **on top of
> [`rit`](../rit)**. This is a **planning artifact** — it does not create app code.
> Today `rithub` is a README scaffold; per its gate it is not built until `rit`'s
> core passes interop / crash-recovery / fuzz / concurrent-ref gates
> ([`../rit/PLAN.md`](../rit/PLAN.md), Release Sequence Stages 5–6). The tree below
> is the forge that gate unlocks: repos, orgs, RBAC, issues, pull requests, review,
> protected branches, webhooks, releases, CI hooks, notifications, audit, and the
> git-smart-HTTP + SSH servers.

---

## 1. Design invariants

- **Built on `rit`, owns no Git internals.** rithub adds *no* object storage,
  refs, packfiles, wire protocol, or merge logic of its own. It path-depends on
  `rit-sdk` and mounts `rit-sdk::server` (upload-pack/receive-pack) behind its
  transports. PR "truth" (mergeability, diffs, conflicts) is computed by `rit`,
  not reimplemented (PLAN: "GitGram does not own object storage, refs, packfiles,
  the Git protocol, pull-request truth, or repository permissions").
- **Single-node first.** Repositories + packfiles on a durable local filesystem;
  **PostgreSQL** for users, orgs, permissions, issues, reviews, jobs, and
  immutable audit events; a transactional **job outbox** for webhooks, email,
  maintenance, indexing (PLAN "Forge architecture"). Multi-node, geo-replication,
  Actions-compatible CI, packages, and LFS are separate later approvals.
- **Adversarial-input & tenancy safety.** Incoming pushes are quarantined until
  verification/connectivity/authorization/policy/size checks pass. Ref changes are
  atomic. Cross-tenant authorization errors, forged webhooks, and unsafe
  Markdown/HTML rendering are in-scope threats (PLAN "Security contract").
- **Domain crates never depend on the HTTP/SSH front end or a specific UI.**
  Persistence sits behind repository-pattern traits so the store is swappable and
  testable.

---

## 2. Target workspace + service tree

`rithub` is its own Cargo workspace (isolated from the desktop build, like `mint`
and `agent-runtime`). It layers `forge-*` domain/service crates over `rit-sdk`.

```text
apps/rithub/
  Cargo.toml                      # [workspace] members = crates/* + services/*
  rust-toolchain.toml
  docs/{THREAT-MODEL,OPERATIONS,BACKUP-RESTORE}.md
  migrations/                     # PostgreSQL schema (sqlx/refinery), append-only
  crates/
    forge-domain/                 # pure domain model + rules, NO db/http:
                                  #   User, Org, Team, Repository, Membership,
                                  #   Issue, PullRequest, Review, Comment, Label,
                                  #   Milestone, Branch, Release, Webhook, Token.
                                  #   Invariants: PR state machine, merge policy.
    forge-rbac/                   # roles, scopes, permission checks, org/team/repo
                                  #   access resolution. The authorization kernel
                                  #   every service calls before an action.
    forge-store/                  # repository-pattern persistence over Postgres
                                  #   (sqlx): repos for each aggregate + the audit
                                  #   event log + tx boundaries. Swappable/mockable.
    forge-git/                    # the rit bridge: wraps rit-sdk::Repository +
                                  #   rit-sdk::server. Repo create/fork/mirror,
                                  #   ref/branch ops, diff/merge for PRs, blame,
                                  #   archive, GC scheduling. THE ONLY crate that
                                  #   touches rit.
    forge-review/                 # PR review domain: diffs (via forge-git→rit-diff),
                                  #   inline threads, suggestions, required-review
                                  #   rules, review state aggregation.
    forge-protect/                # protected branches + merge gates: required
                                  #   checks, required approvals, linear-history,
                                  #   signed-commit enforcement (verified via rit).
    forge-webhooks/               # webhook subscriptions, HMAC signing, delivery
                                  #   records, redelivery. Producer for the outbox.
    forge-notifications/          # notification fan-out (in-app, email), watch/
                                  #   subscription state, digest batching.
    forge-releases/               # releases + tags (via rit tags), release assets
                                  #   (bounded), changelog rendering.
    forge-ci/                     # CI *hooks* (not a runner): status checks API,
                                  #   check-runs, commit-status aggregation,
                                  #   external-runner webhook contract.
    forge-audit/                  # immutable audit event model + query; every
                                  #   privileged action appends here (append-only).
    forge-search/                 # code/issue/PR indexing + query (Tantivy);
                                  #   incremental index jobs fed by the outbox.
    forge-markup/                 # safe Markdown/AsciiDoc rendering, HTML
                                  #   sanitization, syntax highlight, @/#-refs.
    forge-jobs/                   # durable job runtime over the transactional
                                  #   outbox: webhooks, email, maintenance, GC,
                                  #   indexing, notification fan-out. Retries+DLQ.
    forge-api/                    # REST/GraphQL contract types + handlers (the
                                  #   public API surface), request validation.
    forge-sdk/                    # the client SDK + shared contract types other
                                  #   apps import (GitGram, tooling, Railyard).
  services/
    forge-http/                   # axum server: web UI + JSON/GraphQL API +
                                  #   git-smart-HTTP (mounts rit-sdk::server behind
                                  #   auth/RBAC/quarantine). TLS, rate limits.
    forge-ssh/                    # SSH gateway: key-fingerprint auth → git-upload/
                                  #   receive-pack over the SSH channel (rit-sdk).
    forge-worker/                 # runs forge-jobs (outbox consumer); horizontally
                                  #   scalable, idempotent handlers.
    forge-web/                    # server-rendered/SPA front end (repo browser,
                                  #   PR/issue UI, org admin, settings).
  tests/
    e2e/                          # stock `git clone/push` against forge-http +
                                  #   forge-ssh; PR lifecycle; RBAC matrix; tenant
                                  #   isolation; forged-webhook rejection.
```

### Dependency + request flow

```text
forge-web / stock git client
        │  (HTTPS / SSH)
   forge-http / forge-ssh ──auth──▶ forge-rbac
        │                              │
        ├── git transport ──▶ forge-git ──▶ rit-sdk (server + Repository)
        │                              │        └─ quarantine + atomic CAS refs
        └── app actions ──▶ forge-api ─┼─▶ forge-domain (rules)
                                       ├─▶ forge-store (Postgres, tx)
                                       ├─▶ forge-{review,protect,releases,ci}
                                       └─▶ forge-jobs (outbox) ──▶ forge-worker
                                                    └─▶ forge-{webhooks,notifications,search,audit}
```

**How rithub depends on rit:** exactly one crate — `forge-git` — imports
`rit-sdk`. Repo hosting, PR diffs, mergeability, blame, and archive are `rit`
calls; smart-HTTP/SSH is `rit-sdk::server` behind rithub's auth + RBAC +
quarantine. If `rit` isn't past its compat gate, rithub cannot host real repos —
which is precisely why the gate blocks the forge.

---

## 3. Phased build order

Begins only after `rit` Stage 4 (`rit` can clone/fetch/push/merge). Maps to PLAN
Stage 5 (single-node forge) → Stage 6 (public production).

| Phase | Delivers | Exit gate |
|-------|----------|-----------|
| **F0** Foundation | `forge-domain`, `forge-store` (+migrations), `forge-rbac`, `forge-git` bridge | Create/fork a repo; stock `git clone/push` over `forge-http` with RBAC + quarantine |
| **F1** Collaboration | `forge-review`, PR/issue in `forge-api`, `forge-markup`, `forge-web` MVP | Full PR lifecycle: open → review → merge (mergeability from `rit`) |
| **F2** Governance | `forge-protect`, `forge-audit`, scoped tokens + SSH keys, operator MFA | Protected branches enforced; every privileged action audited |
| **F3** Automation | `forge-webhooks`, `forge-notifications`, `forge-ci` hooks, `forge-jobs`/`forge-worker` outbox | Signed webhooks delivered w/ retry; external CI reports status checks |
| **F4** Scale-out | `forge-search`, `forge-releases`, backup/restore drills, capacity tests | Coordinated FS-snapshot + Postgres restore proven; search incremental |
| **F5** Public | security review, privacy/terms, incident response, upgrade/rollback | PLAN Stage 6 approvals all pass |

---

## 4. Graph engineering layer

The forge domain is a knowledge graph, and its core workflows are task graphs.
Modeling both (per `~/.claude/skills/graph-engineering/references/`) is what keeps
RBAC, audit, and notifications coherent instead of ad-hoc.

### 4.1 Forge knowledge graph (ontology)

**Competency questions:** Who can merge to branch `B` of repo `R`? Which PRs target
`B` and are blocked on a required review? What did user `U` do in org `O` last
week (audit)? Which repos belong to org `O` and who are their admins? Which issues
does PR `P` close?

**Entities:** `User`, `Org`, `Team`, `Repository`, `Branch`, `Issue`,
`PullRequest`, `Review`, `Comment`, `Release`, `Webhook`, `Token`, `AuditEvent`.
(`Branch`, `Commit`, `Tag` are *projections of the `rit` object graph* — rithub
does not re-store them; it references `rit` object ids.)

**Relations (verb, domain → range, with time + provenance on every edge):**

| Relation | Domain → Range |
|----------|----------------|
| `MEMBER_OF` | User → Org / Team (role attr) |
| `OWNS` / `BELONGS_TO` | Org → Repository |
| `AUTHORS` | User → {Issue, PullRequest, Comment, Review} |
| `TARGETS` | PullRequest → Branch |
| `SOURCES_FROM` | PullRequest → Branch |
| `IN` | Branch → Repository |
| `REVIEWS` | Review → PullRequest (state attr) |
| `CLOSES` | PullRequest → Issue |
| `PROTECTS` | ProtectionRule → Branch |
| `SUBSCRIBES` | Webhook → Repository (event mask) |
| `PERFORMED` | User → AuditEvent → {any entity} |

The canonical worked chain from the coordinator:
`User —AUTHORS→ PullRequest —TARGETS→ Branch —IN→ Repository —BELONGS_TO→ Org`.
**Time + provenance ride every edge** (who, when, request id, ip) — this is not
decoration: it *is* `forge-audit`. The append-only `AuditEvent` node with
`PERFORMED` edges is a provenance sub-graph over the whole domain, and `forge-rbac`
is a reachability query over `MEMBER_OF`/`OWNS`/team edges ("is there an authorizing
path from `U` to action on `R`?"). Storing provenance at modeling time (not
retrofitted) is exactly the modeling rule; the append-only log guarantees it.

### 4.2 PR-review / CI as a task graph (the diamond + human gate)

A PR merge is the canonical diamond with a real human gate on the irreversible edge:

```text
              ┌─ CI check: build ───┐
              ├─ CI check: tests ───┤
open PR ──────┼─ CI check: lint ────┼─→ VERIFY (forge-protect aggregates:
   │          ├─ security scan ─────┤     all required checks green?)
   │          └─ rit: mergeability ─┘            │
   └──────────── required reviews ───────────────┤
                                                 ▼
                              [HUMAN GATE: required approvals reached]
                                                 ▼
                              owned merge (forge-git → rit atomic CAS ref)
                                                 ▼
                              fan-out (outbox): webhooks, notifications, audit
```

- **Parallel workers** = independent CI checks + security scan + rit mergeability;
  they never read each other's output → true fan-out ("delete fake edges").
- **Separate verify node** = `forge-protect` aggregating required checks — never the
  same context that produced a check grading itself.
- **Human gate** placed on the *irreversible* edge (merge to a protected branch),
  not on every step (PLAN: protected branches, required reviews).
- **One owner of the merge** = `forge-git`'s single atomic CAS ref update in `rit`
  — no two writers to a branch (stop-rule + one-writer guardrail).
- **Guardrails:** webhook redelivery has max rounds + DLQ; outbox handlers are
  idempotent; the merge is transactional with the audit append.

---

## 5. Enterprise scale estimate

**Today `rithub` is a single README.** A real self-hostable forge is a
*thousands-of-files* product. For calibration: Gitea is ~500K+ LOC (Go+templates),
GitLab is millions (Ruby/Vue). A focused, single-node rithub that reaches Gitea-
class functionality is realistically **1,500–3,000 source files and 250K–450K LOC**
across backend crates, migrations, API, and web front end.

| Crate / service | ~Files | ~LOC |
|-----------------|-------:|-----:|
| forge-domain | 40 | 14,000 |
| forge-rbac | 15 | 6,000 |
| forge-store (+migrations) | 60 | 18,000 |
| forge-git (rit bridge) | 25 | 10,000 |
| forge-review | 25 | 11,000 |
| forge-protect | 15 | 6,000 |
| forge-webhooks | 15 | 5,500 |
| forge-notifications | 18 | 6,500 |
| forge-releases | 12 | 4,500 |
| forge-ci (hooks) | 18 | 7,000 |
| forge-audit | 10 | 3,500 |
| forge-search | 20 | 9,000 |
| forge-markup | 15 | 6,000 |
| forge-jobs | 18 | 7,500 |
| forge-api | 70 | 26,000 |
| forge-sdk | 25 | 8,000 |
| forge-http (service) | 45 | 16,000 |
| forge-ssh (service) | 15 | 6,000 |
| forge-worker (service) | 12 | 4,500 |
| forge-web (front end) | 250 | 55,000 |
| tests/e2e | 60 | 18,000 |
| **Backend+SDK subtotal** | **~500** | **~185K** |
| **+ web front end + tests** | **~810** | **~258K** |

**What "done" means per phase:** F0 ≈ 150 files; F1 (collaboration) ≈ +250;
F2–F3 (governance + automation) ≈ +300; F4–F5 ≈ +100. A README is **~0.1%** of the
target. rithub is the largest app in this trio by an order of magnitude — nobody
should mistake the scaffold, or even F0, for the forge.

---

## 6. SDK / interop & platform composability

**`forge-sdk`** is rithub's stable client + contract surface (REST/GraphQL types,
webhook payload schemas, HMAC verification helpers). It re-exports the `rit-sdk`
object/id types so a consumer can resolve a PR/commit reference down to real Git
objects.

Composability with Railyard — **honest fit**:

- **rithub → rit:** a hard code dependency (`forge-git` → `rit-sdk`). This is the
  cleanest composition in the trio: the forge *is* rit-plus-collaboration.
- **rithub ↔ GitGram / external:** public authenticated API + signed webhooks
  (PLAN) — the same seam GitHub exposes; GitGram integrates over it, owning no Git.
- **rithub ↔ ranguard:** rithub's CI-hook and worker agents can be **attested by
  [`ranguard`](../ranguard)** (its process-attestation + signed-manifest SDK), so
  the forge only trusts runners whose binary hash + manifest verify. Optional,
  additive.
- **rithub ↔ Mint / Passport (payment rail):** **rithub is a developer tool, not a
  payer on the AITP-402 rail.** It settles no money. The only honest tie is
  *provenance*: a release or merge commit id can be recorded in a **context-passport**
  version or referenced by a **Mint** hash-linked receipt ("artifact produced at
  commit `<oid>`"). That binding lives in Passport/Mint, never inside rithub. Do
  not manufacture a billing coupling that the domain doesn't need.

---

## 7. Key decisions

1. **One bridge crate to rit** (`forge-git`) — the entire Git dependency is
   isolated and mockable; the rest of the forge is pure domain + Postgres.
2. **Postgres for metadata, filesystem for repos, outbox for side effects** —
   straight from PLAN's single-node contract; no Workers/R2 as the repo plane.
3. **RBAC + audit are graph queries** over one domain model — authorization and
   accountability share the entity graph instead of drifting apart.
4. **PR merge is a diamond with a human gate on the merge edge** — parallel checks,
   separate verifier (`forge-protect`), one owning atomic ref update in rit.
5. **No payment-rail coupling** — provenance ties to Mint/Passport are additive and
   consumer-owned; rithub stays a systems/dev tool.
