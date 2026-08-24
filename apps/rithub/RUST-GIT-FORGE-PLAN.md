# Rust Git Forge Product Plan

Last revised: 23 August 2026

## Decision record

This is a production open-source systems product, not GitGram and not a
hackathon implementation. Its eventual repository boundary is
`videogen/apps/<approved-name>/`. The neutral working name is **Rust Git Forge**
until brand and package-name checks are complete.

No files are created under `videogen/apps` until the `videogen` issue tracker can
create and claim the work item. This workspace plan preserves the approved
boundary without bypassing repository governance.

## Product boundary

Rust Git Forge has two layers:

1. A library-first Rust implementation of Git repositories and the Git wire
   protocol, compatible with existing Git repositories and clients.
2. A self-hostable collaboration forge built on that implementation, including
   repositories, organizations, permissions, issues, pull requests, review,
   protected branches, webhooks, releases, and audit logs.

GitGram remains a mostly TypeScript social product. It turns repositories into
Instagram-like accounts with posts, reels, stories, media publishing,
followers, and feeds. GitGram does not own object storage, refs, packfiles, the
Git protocol, pull-request truth, or repository permissions. It integrates with
GitHub and, later, Rust Git Forge through public authenticated APIs and webhooks.

## Initial customer and promise

The initial user is an engineer or small team that needs a trustworthy local
Git implementation with readable internals and strong compatibility tests. The
first production promise is deliberately narrower than a complete GitHub
replacement: users can inspect, create, verify, exchange, and recover standard
Git repositories without corrupting data.

The forge becomes a supported product only after the core implementation passes
interoperability, crash-recovery, parser-fuzzing, and concurrent-ref tests.

## Compatibility scope

The core implementation must cover:

- loose blobs, trees, commits, annotated tags, object verification, and object
  traversal;
- SHA-1 repository compatibility with collision detection, plus an explicit
  path to Git's SHA-256 repository format;
- refs, symbolic refs, packed refs, reflogs, atomic compare-and-swap updates,
  and lock recovery;
- index formats, worktrees, ignore rules, status, add, restore, and commit;
- revision parsing, graph walks, merge bases, diffs, rename heuristics, and
  conflict representation;
- pack indexes, packfiles, delta resolution, reachability, negotiation, and
  quarantine of received objects;
- smart protocol v2 over HTTP and SSH for clone, fetch, and push;
- three-way merge, rebase, cherry-pick, revert, and deterministic conflict
  handling.

Submodules, sparse checkout, partial clone, large-file storage, replace refs,
notes, and advanced maintenance enter the compatibility matrix explicitly. None
is silently claimed before its test suite passes.

## Rust workspace architecture

The intended crate boundaries are:

```text
crates/
  git-object       object IDs, parsing, validation, canonical serialization
  git-odb          loose and packed object databases, quarantine, alternates
  git-refs         refs, reflogs, transactions, locks
  git-index        index formats and extensions
  git-worktree     checkout, status, ignore rules, filesystem safety
  git-revision     revision syntax, graph traversal, reachability
  git-diff         text and binary diff, rename detection
  git-merge        merge bases, strategies, conflicts, sequencer
  git-pack         pack/index parsing, deltas, negotiation inputs
  git-protocol     protocol v2, upload-pack, receive-pack transports
  git-cli          compatibility-oriented command-line interface
  forge-domain     users, organizations, repositories, issues, reviews
  forge-store      metadata persistence and repository coordination
  forge-http       browser/API and smart-HTTP server
  forge-ssh        SSH command gateway and key authorization
  forge-jobs       durable maintenance, hooks, webhooks, notifications
```

Shared crates cannot depend on the forge UI or a specific database. Parsing and
storage APIs expose bounded streams rather than assuming that objects or packs
fit in memory. Platform-specific filesystem behavior stays behind explicit
adapters and platform tests.

## Forge architecture

The first forge is a single-node, self-hosted service:

- repositories and packfiles on a durable local filesystem;
- PostgreSQL for users, organizations, permissions, issues, reviews, jobs, and
  immutable audit events;
- an SSH service for Git transport and an HTTP service for API, browser, and
  smart HTTP;
- a transactional job outbox for webhooks, email, repository maintenance, and
  indexing;
- filesystem snapshots plus PostgreSQL backups with a tested coordinated
  restore procedure.

SQLite may support the desktop or single-user mode, but it is not used to imply
safe multi-node repository coordination. Multi-node storage, geo replication,
packages, runners, Actions-compatible CI, and large-file storage are later
designs with separate threat and cost reviews.

Cloudflare Workers are not the Git execution or storage plane. Large pack
streams, SSH, repository filesystem semantics, and long-running maintenance need
a local process or conventional server. Cloudflare may serve static project
documentation and, after measurement, proxy ordinary web traffic. R2 is only a
future cold-object or backup option under the shared account cost policy.

## Security contract

Repository data is adversarial input. The product must defend against:

- malformed object, index, pack, bitmap, commit-graph, and protocol data;
- path traversal, case-folding collisions, NTFS/HFS hazards, unsafe symlinks,
  and checkout outside the worktree;
- decompression bombs, delta chains, integer overflow, unbounded graph walks,
  resource exhaustion, and archive bombs;
- SHA-1 collision attacks and object-ID confusion between hash formats;
- ref races, lock theft, non-fast-forward policy bypass, hook injection, and
  partially received pushes;
- cross-tenant authorization errors, leaked deploy keys or tokens, forged
  webhooks, and unsafe HTML or Markdown rendering.

Incoming pushes remain quarantined until object verification, connectivity,
authorization, policy, and size checks pass. Ref changes use atomic
transactions. The forge supports scoped tokens, SSH key fingerprints,
phishing-resistant operator MFA, protected branches, signed commit and tag
verification, secret redaction, security logs, and auditable administrative
actions.

## Verification strategy

Production claims require retained evidence from:

- differential tests that run equivalent operations through Rust Git Forge and
  the reference Git CLI, then compare repository state and command semantics;
- interoperability matrices across supported Git versions, operating systems,
  filesystem behaviors, HTTP servers, and SSH clients;
- property tests for serialization, graph traversal, deltas, refs, and index
  round trips;
- coverage-guided fuzzing for every untrusted parser and protocol state machine;
- Git project test fixtures where licensing and harness boundaries permit;
- fault injection around writes, fsync, locks, process death, partial network
  reads, interrupted clone/fetch/push, and coordinated restore;
- concurrency tests for ref updates, simultaneous pushes, garbage collection,
  and read/write isolation;
- independent security review before exposing public repository hosting.

Every compatibility claim is tied to a named test. A repository fixture that
fails remains a release blocker or a documented unsupported capability.

## Release sequence

### Stage 0: specification and harness

Publish the compatibility matrix, threat model, repository fixtures,
differential harness, fuzz targets, performance budgets, and corruption-recovery
rules.

### Stage 1: read-only repository core

Ship object inspection, `cat-file`, `ls-tree`, log traversal, refs, pack reads,
and `fsck`. Prove that opening hostile repositories cannot write outside the
repository or exhaust unbounded resources.

### Stage 2: local authoring

Ship init, status, add, diff, commit, branch, checkout/restore, tag, reflog, and
garbage-safe object writes. Pass crash tests for every state-changing command.

### Stage 3: exchange

Ship protocol v2 clone, fetch, and push over HTTP and SSH. Prove quarantine,
atomic refs, authentication, authorization, interrupted transfer recovery, and
interop with the reference Git client and server.

### Stage 4: history editing

Ship merge, rebase, cherry-pick, revert, conflict continuation/abort, and
recovery. Preserve user work during failures and match documented Git behavior.

### Stage 5: single-node forge

Ship accounts, organizations, repository permissions, issues, pull requests,
review, branch protection, webhooks, releases, operator tools, backups, restore,
export, deletion, abuse handling, and upgrade/rollback procedures.

### Stage 6: public production

Onboard outside repositories only after security review, capacity tests, restore
drills, support procedures, privacy/terms, observability, incident response, and
an upgrade from the previous release all pass. Multi-node and CI features remain
separate approvals.

## Non-goals for the first public release

- replacing every Git command at once;
- claiming GitHub parity;
- running arbitrary CI jobs;
- hosting unbounded public binary releases or package registries;
- coupling repository correctness to GitGram feeds;
- storing primary repositories in browser storage, R2, or a Worker runtime;
- inventing a new incompatible wire protocol before Git interoperability works.

## Pre-code gate

Before implementation under `videogen/apps`:

1. repair the `videogen` tracker and create/claim the product issue;
2. approve the project name, license, governance, supported platforms, and MSRV;
3. freeze the Stage 1 compatibility matrix and threat model;
4. select reference Git versions and reusable test fixtures after license
   review;
5. decide whether existing Rust Git crates are comparison subjects,
   dependencies, or clean-room references;
6. define benchmark repositories and memory, CPU, disk, and network budgets;
7. approve the single-node data and backup contract without using Cloudflare as
   the repository execution plane.
