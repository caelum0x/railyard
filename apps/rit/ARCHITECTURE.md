# rit — Enterprise Architecture

> **Scope of this document.** The *target* enterprise shape for `rit`: the full
> Git implementation as a multi-crate Rust workspace, the phased build order that
> maps the [`PLAN.md`](./PLAN.md) deferred scope into crates, and the SDK/interop
> surface other Railyard apps (chiefly [`rithub`](../rithub)) build on. This is a
> **planning artifact** — it does not change app code. Today `rit` is a single
> crate (`src/{object,odb,oid,refs,repo,blob,tree,commit,tag,signature,headers,error}.rs`,
> 35 tests incl. real-git interop against `git` 2.50). The target below decomposes
> that core and lands the deferred scope (packfiles/delta, index/worktree, wire
> protocol v2, merge/rebase, SHA-256) as discrete crates behind a hard
> compatibility gate.

---

## 1. Design invariants (carried from PLAN.md)

These are non-negotiable and shape every crate boundary:

- **Library-first.** Every capability lives in a library crate; the CLI is a thin
  driver. No behavior is CLI-only.
- **Real-git-compat gate.** No compatibility claim ships without a named test.
  Object ids are byte-identical to `git hash-object`; `git fsck` accepts what we
  write; every parser has a differential test against the reference `git` CLI.
  **The forge (`rithub`) is not built until the `rit` core passes interop,
  crash-recovery, parser-fuzzing, and concurrent-ref gates** (PLAN "Initial
  customer and promise" + "Release sequence").
- **Adversarial input.** Repository data is hostile. Every parser is fail-closed,
  bounded (the `MAX_OBJECT_SIZE` cap and `ZlibDecoder::take` guard in today's
  `odb.rs` are the template), and fuzzed.
- **Shared crates cannot depend on the forge, a UI, or a specific database**
  (PLAN "Rust workspace architecture"). Storage APIs expose bounded streams, not
  "assume it fits in memory."
- **Hash-agnostic core.** The object model is generic over the hash so the
  SHA-1 → SHA-256 path is a type parameter, not a rewrite.
- **Immutability.** Objects are content-addressed and never mutated in place;
  writes are atomic (temp file → `fsync` → rename, as `Odb::write` already does).

---

## 2. Target workspace tree

The standalone `rit` package becomes a Cargo **workspace** (it keeps
`[workspace]` isolation from the desktop GPUI/Metal build, exactly like `mint`
and `agent-runtime` are excluded at the repo root). Crates use the `rit-` prefix;
the PLAN's `git-*` names are noted as aliases.

```text
apps/rit/
  Cargo.toml                      # [workspace] members = crates/*
  rust-toolchain.toml             # pinned MSRV (PLAN pre-code gate item 1)
  README.md
  PLAN.md                         # product + release plan (source of truth)
  ARCHITECTURE.md                 # this document
  deny.toml                       # cargo-deny: license/advisory gate
  docs/
    COMPATIBILITY.md              # the frozen compat matrix (Stage 0 deliverable)
    THREAT-MODEL.md               # adversarial-input threat model (Stage 0)
    RECOVERY.md                   # corruption/crash-recovery rules (Stage 0)
  fixtures/                       # licensed real-git repos for differential tests
  crates/
    rit-hash/                     # [git-hash] hash abstraction: Sha1, Sha256,
                                  #   sha1dc collision detection, ObjectId<H>,
                                  #   hex, prefix. Everything else is generic
                                  #   over this so SHA-256 is a type param.
    rit-object/                   # [git-object] object model: ObjectType, loose
                                  #   framing "<type> <len>\0<payload>", canonical
                                  #   (de)serialization for blob/tree/commit/tag,
                                  #   validation. (today's object/blob/tree/
                                  #   commit/tag/signature/headers.rs)
    rit-odb/                      # [git-odb] object database: loose store (today's
                                  #   odb.rs) + packed store, quarantine dirs,
                                  #   alternates, streaming reads, fsck-grade
                                  #   verification. Facade over rit-pack + loose.
    rit-pack/                     # [git-pack] packfile + .idx parsing, ref/ofs
                                  #   delta resolution, thin-pack fixups, pack
                                  #   writing, reachability bitmaps, delta chain
                                  #   bounds. (PLAN deferred: packfiles/delta)
    rit-refs/                     # [git-refs] refs, symbolic refs, packed-refs
                                  #   (today's refs.rs) + reflogs, atomic
                                  #   compare-and-swap transactions, lock files,
                                  #   lock recovery, ref races.
    rit-index/                    # [git-index] index (staging) formats v2/3/4 +
                                  #   extensions (TREE, UNTR, sparse). (PLAN
                                  #   deferred: index/worktree)
    rit-worktree/                 # [git-worktree] checkout, status, add, restore,
                                  #   .gitignore rules, filesystem-safety adapters
                                  #   (path traversal, case-fold, symlink hazards).
    rit-revision/                 # [git-revision] rev syntax (HEAD~3, a..b, ^),
                                  #   graph walks, reachability, merge-base — the
                                  #   bounded walk in today's Repository::log,
                                  #   generalized.
    rit-diff/                     # [git-diff] Myers/histogram text diff, binary
                                  #   diff, rename/copy detection, hunk model.
    rit-merge/                    # [git-merge] merge bases, three-way merge,
                                  #   recursive/ort strategy, conflict model, the
                                  #   sequencer for rebase/cherry-pick/revert.
    rit-protocol/                 # [git-protocol] smart protocol v2, pkt-line,
                                  #   capability negotiation, upload-pack (fetch/
                                  #   clone) + receive-pack (push), quarantine of
                                  #   received objects. Transport-agnostic.
    rit-transport/               #   HTTP + SSH transport adapters for rit-protocol
                                  #   (client side): git-smart-HTTP, ssh exec.
    rit-fsck/                     # object/connectivity/reachability verification,
                                  #   corruption reporting — the read-only "is this
                                  #   repo sound?" pass used by Stage 1 + CI.
    rit-repo/                     # [porcelain] the Repository facade (today's
                                  #   repo.rs) tying odb+refs+index+worktree into
                                  #   init/add/commit/log/branch/checkout/merge.
    rit-porcelain/               #   high-level command semantics matching git's
                                  #   UX (status output, log formatting) — what the
                                  #   CLI and SDK both call.
    rit-cli/                      # [git-cli] the `rit` binary: thin clap driver
                                  #   over rit-porcelain (today's main.rs,
                                  #   hash-object/cat-file/ls-tree/log → grows to
                                  #   init/add/commit/branch/clone/fetch/push).
    rit-sdk/                      # the stable, semver'd public API surface other
                                  #   apps depend on (rithub, tooling). Re-exports
                                  #   a curated subset; hides crate churn.
    rit-testkit/                  # differential harness: shell out to real `git`,
                                  #   assert byte-equality (today's tests/interop.rs
                                  #   promoted to a reusable crate), fixture repos,
                                  #   property-test generators.
    rit-fuzz/                     # cargo-fuzz targets: every untrusted parser
                                  #   (loose, tree, commit, pack, idx, pkt-line,
                                  #   index, packed-refs) + protocol state machines.
```

### Crate dependency graph (acyclic, leaf → root)

```text
rit-hash
  └─ rit-object
       ├─ rit-pack ──┐
       ├─ rit-odb ◄──┘        (odb is the facade over loose + pack)
       ├─ rit-refs
       ├─ rit-index
       ├─ rit-diff
       ├─ rit-revision (← odb, refs)
       ├─ rit-merge    (← diff, revision, odb)
       ├─ rit-worktree (← odb, index, refs)
       ├─ rit-fsck     (← odb, refs, revision)
       └─ rit-protocol (← odb, refs, pack)
             └─ rit-transport (HTTP/SSH)
rit-repo   (← odb, refs, index, worktree, revision, merge, protocol)
rit-porcelain (← rit-repo)
rit-cli    (← rit-porcelain)          rit-sdk (← curated re-exports)
rit-testkit / rit-fuzz  (dev-only, depend across the graph)
```

**Boundary rules enforced by CI:** `rit-hash`/`rit-object` depend on nothing
Railyard-specific; nothing below `rit-repo` knows about a CLI; no crate depends on
a database or on `rithub`. `cargo-deny` guards the license/advisory surface (PLAN
"Verification strategy").

---

## 3. Phased build order (maps PLAN Release Sequence → crates)

Each stage is gated: it does not start until the prior stage's named tests pass.

| Stage | PLAN name | Crates delivered / extended | Gate to exit |
|------|-----------|------------------------------|--------------|
| **0** | Specification & harness | `rit-testkit`, `rit-fuzz`, `docs/{COMPATIBILITY,THREAT-MODEL,RECOVERY}.md`, `fixtures/` | Compat matrix + threat model frozen; differential harness green on the existing 4 commands |
| **1** | Read-only core | Split today's crate into `rit-hash` `rit-object` `rit-odb` `rit-refs` `rit-revision` `rit-repo` `rit-porcelain` `rit-cli`; add `rit-pack` (read), `rit-fsck` | `cat-file`/`ls-tree`/`log`/`fsck` on hostile repos never write outside the repo or use unbounded resources; pack **reads** interop-verified |
| **2** | Local authoring | `rit-index`, `rit-worktree`, `rit-diff`; extend `rit-repo`/`rit-cli` with init/status/add/restore/commit/branch/tag/reflog | Crash tests pass for every state-changing command; index round-trips vs real git |
| **3** | Exchange | `rit-protocol` (upload-pack + receive-pack), `rit-transport` (HTTP+SSH), `rit-pack` (write/thin), quarantine in `rit-odb` | clone/fetch/push interop with reference git client **and** server; atomic refs + quarantine + interrupted-transfer recovery proven |
| **4** | History editing | `rit-merge` (three-way, sequencer), rebase/cherry-pick/revert/conflict continue-abort in `rit-repo`/`rit-cli` | Merge/rebase match documented git behavior; user work preserved across induced failures |
| **5→6** | Forge / public | *No new `rit` crates* — this is where [`rithub`](../rithub) consumes `rit-sdk`. `rit`'s SHA-256 repository format (`rit-hash` second hash) lands here as an explicit, separately-gated matrix entry. | Security review; SHA-256 interop matrix; capacity + restore drills (owned by rithub) |

**SHA-256** is intentionally *not* a late rewrite: `rit-hash` is generic from
Stage 1, so SHA-256 is a new hash implementation + a compat-matrix row, gated at
Stage 5/6 per PLAN ("explicit path to Git's SHA-256 repository format").

---

## 4. SDK / interop surface

`rit-sdk` is the **only** crate external consumers (rithub, internal tooling) are
allowed to depend on. It is semver-stable and hides the churn of the inner crates.

**Rust API contract (the shape rithub builds on):**

```text
rit_sdk::Repository        open / discover / init  (today's Repository, widened)
  .odb() -> Odb            content-addressed object read/write, streaming
  .refs() -> RefStore      atomic CAS ref transactions, reflog, packed-refs
  .index()/.worktree()     staging + checkout (Stage 2+)
  .revwalk(spec)           bounded graph traversal / merge-base (rit-revision)
  .diff(a, b)/.merge(...)  (Stage 4)
rit_sdk::server            upload_pack / receive_pack handlers over an
                           AsyncRead/AsyncWrite pair — the seam a forge mounts
                           behind smart-HTTP and SSH (Stage 3)
rit_sdk::ObjectId / Object / Signature   canonical types, hash-generic
```

**Wire/tooling interop (beyond the Rust API):**

- **Git wire protocol v2** — `rit-protocol` speaks the real thing, so *any* stock
  `git` client can clone/fetch/push against a server embedding `rit-sdk::server`.
  This is the interop guarantee that lets rithub host real repos for real users.
- **On-disk format** — the `.git` directory `rit` reads/writes is byte-compatible,
  so a rithub-hosted repo is inspectable by stock `git` and vice-versa.
- **`rit` CLI** — a scriptable boundary for CI hooks and ops tooling.

---

## 5. Composability with the Railyard platform

`rit` is the **core dependency of `rithub`**: rithub adds no object storage, refs,
packfiles, protocol, or PR truth of its own — it path-depends on `rit-sdk` and
mounts `rit-sdk::server` behind its HTTP/SSH front doors (see
[`../rithub/ARCHITECTURE.md`](../rithub/ARCHITECTURE.md) §"How rithub depends on
rit").

Honest fit with the rest of the rail:

- **`rit` is a developer/systems tool, not part of the AITP-402 payment rail.**
  It has no `Mint` dependency and settles no money. That separation is deliberate
  and correct — coupling repository correctness to a ledger would violate the PLAN
  security contract.
- **Provenance touch-points (opt-in, additive):** `rit`'s content-addressed
  object ids and signed-commit/tag verification (`rit-object` signatures) are a
  natural anchor for provenance. A commit id can be referenced by a **Mint**
  hash-linked receipt or a **context-passport** version record so "this artifact
  was produced at commit `<oid>`" is verifiable — but that binding lives in the
  *consumer* (Passport/Mint), never inside `rit`. `rit` stays a pure Git core.
- **AITimeMachine** may attach checkpoints to commit ids; again, `rit` exposes the
  id, the consumer owns the mapping (PLAN "AITimeMachine … does not implement or
  replace Git").

---

## 6. Graph engineering layer

Git *is* a knowledge graph: the object database is a content-addressed DAG, and
refs are named roots over it. Modeling `rit` as a graph (per
`~/.claude/skills/graph-engineering/references/modeling.md`) makes the ontology
explicit and gives `rit-fsck`, `rit-revision`, and `rithub`'s code-intelligence a
shared vocabulary.

### 6.1 Ontology (the object DAG)

**Competency questions** (the schema's spec + test suite):
1. Which commits are reachable from ref `R`?
2. What is the merge-base of commits `A` and `B`?
3. Which blobs/trees does commit `C` introduce vs. its parents?
4. Who authored, and who committed, `C`, and when?
5. Which tag objects point (transitively) at commit `C`?
6. Is object `O` reachable from any ref (i.e. safe from GC)?

**Entities** (canonical name = lowercase 40/64-hex object id; refs by full name):

| Entity | Definition | Backing type |
|--------|-----------|--------------|
| `Blob` | file contents (a leaf) | `rit-object::Blob` |
| `Tree` | directory listing | `rit-object::Tree` |
| `Commit` | a history node | `rit-object::Commit` |
| `Tag` | annotated tag | `rit-object::Tag` |
| `Ref` | named mutable pointer (branch/tag/HEAD) | `rit-refs::RefTarget` |
| `Signature` | *attribute-bearing* author/committer identity (name, email, time, tz) | `rit-object::Signature` |

**Relations** (precise verbs, `domain → range`, with time + provenance):

| Relation | Domain → Range | Provenance / time carried |
|----------|----------------|---------------------------|
| `HAS_PARENT` | Commit → Commit | ordered (first-parent); defines history edges |
| `HAS_TREE` | Commit → Tree | root tree of the snapshot |
| `CONTAINS` | Tree → {Tree, Blob} | entry `name` + `mode` as edge attrs |
| `POINTS_AT` | Ref → {Commit, Tag} | via reflog: who moved it, old→new oid, time |
| `TAGS` | Tag → {Commit, Tree, Blob, Tag} | tagger `Signature` |
| `AUTHORED` / `COMMITTED` | Signature → Commit | **time + tz are the edge's timestamp** |

Provenance is intrinsic here — it is not retrofitted metadata but the object bytes
themselves: author/committer signatures and reflog `who/old/new/when` lines *are*
the edge properties, and every id is a cryptographic content hash, so the graph is
tamper-evident by construction. This is exactly the "decide how every fact carries
time + provenance at modeling time" rule, satisfied for free by Git's design.

`rit-revision` is the query engine over this graph (reachability, merge-base,
`a..b`), and `rit-fsck` is the consistency checker (every edge's range object must
exist and hash-verify — connectivity). The bounded visited-set walk already in
`Repository::log` is the template: no unbounded graph walks on adversarial input.

### 6.2 Task graphs (fetch / push / merge)

Exchange and history-editing operations are naturally task graphs (per
`task-graphs.md`), and modeling them that way is what lets `rit-protocol` and
`rit-odb` parallelize safely without corrupting data.

**Fetch/clone — the diamond.** Negotiation produces a set of wanted objects that
never read each other's bytes → the "delete fake edges" win: object/pack transfer
fans out in parallel, then a mandatory **separate verify node** (quarantine) gates
the merge into the live odb:

```text
              ┌─ transfer pack slice 1 ─┐
negotiate ────┼─ transfer pack slice 2 ─┼─→ verify (fsck + connectivity,   ─→ install
(wants/haves) └─ transfer pack slice 3 ─┘   in QUARANTINE odb)                refs (CAS)
```

The verify node is non-negotiable (a peer's pack is adversarial input): delta
chains resolved, every object hash-checked, connectivity proven — *before* any ref
moves. This is quarantine, straight from PLAN's security contract.

**Push — diamond + human/policy gate.** Same parallel receive → quarantine verify,
but the ref-update edge is *irreversible on a shared repo*, so it routes through an
explicit gate:

```text
receive-pack ─→ quarantine verify ─→ [POLICY/HUMAN GATE: protected-branch,
                                       non-fast-forward, signed-commit, hooks]
                                     ─→ atomic CAS ref transaction (owned merge)
```

`rit` exposes the gate as a hook/policy seam; **rithub** supplies the human side
(branch protection, required reviews — see its task-graph section). One owner of
the ref merge (the atomic CAS transaction in `rit-refs`) — never two writers,
satisfying the "one writer per artifact" guardrail.

**Merge — bounded diamond.** merge-base (parallel ancestry walks from each tip) →
three-way content merge per path (parallelizable, independent files) → verify (no
conflict markers left / conflicts surfaced) → single sequencer owns the resulting
commit. The sequencer in `rit-merge` is the single merge-owner the stop rule
demands; every rebase/cherry-pick loop carries a max-round cap.

---

## 7. Enterprise scale estimate

**The current `rit` is ~14 source files. That is the loose-object read slice, NOT
a complete Git.** A complete Git implementation (packfiles + delta, index,
worktree, wire protocol v2, diff, merge/rebase, SHA-256) is *hundreds* of source
files and six figures of Rust. For calibration: libgit2 is ~100K LOC of C and
gitoxide (the mature Rust Git) spans ~30 crates and well over 200K lines. The
estimate below is **sizing, not empty scaffolding** — it exists so no one mistakes
today's slice for a finished product.

| Crate | ~Files | ~LOC | Notes |
|-------|-------:|-----:|-------|
| rit-hash | 3 | 800 | SHA-1, SHA-256, sha1dc, generic id |
| rit-object | 12 | 4,000 | *(today's object/blob/tree/commit/tag lives here)* |
| rit-odb | 8 | 3,500 | *(today's odb.rs)* + packed facade, quarantine, alternates |
| rit-pack | 20 | 12,000 | packfile/.idx, delta resolution, bitmaps, pack writing |
| rit-refs | 10 | 4,500 | *(today's refs.rs)* + reflog, CAS transactions, locks |
| rit-index | 12 | 6,000 | index v2/3/4 + extensions |
| rit-worktree | 15 | 8,000 | checkout, status, ignore, fs-safety adapters |
| rit-revision | 10 | 5,000 | rev syntax, graph walks, merge-base |
| rit-diff | 12 | 7,000 | Myers/histogram, rename/copy detection |
| rit-merge | 14 | 9,000 | three-way, ort strategy, sequencer |
| rit-protocol | 18 | 11,000 | v2, pkt-line, upload-pack, receive-pack |
| rit-transport | 10 | 5,000 | HTTP + SSH adapters |
| rit-fsck | 6 | 2,500 | object/connectivity verification |
| rit-repo | 12 | 6,000 | *(today's repo.rs)* widened to full porcelain |
| rit-porcelain | 20 | 9,000 | command semantics (status/log/diff formatting) |
| rit-cli | 25 | 6,000 | *(today's main.rs)* → one module per subcommand |
| rit-sdk | 4 | 1,200 | stable public surface |
| rit-testkit | 10 | 4,000 | *(today's tests/interop.rs promoted)* + fixtures |
| rit-fuzz | 12 | 2,000 | cargo-fuzz targets per parser |
| **Total (src)** | **~233** | **~106,500** | +tests/fixtures → **~300–400 files, ~150K LOC** |

**What "done" means per phase:** Stage 1 (read-only core) ≈ 60–80 files. Stage 2
(local authoring) ≈ +80 files. Stage 3 (exchange) ≈ +70 files. Stage 4 (history
editing) ≈ +50 files. The **current 14-file slice is ~5% of a complete `rit`** and
corresponds to a *portion* of Stage 1. Declaring `rit` "done" at the loose-object
slice would be declaring Git done at `cat-file`.

---

## 8. Key decisions

1. **Workspace, not one crate** — decompose along PLAN's crate lines so the
   deferred scope (pack/index/worktree/protocol/merge) lands as isolated,
   independently-testable, independently-fuzzed units.
2. **`rit-hash` generic from day one** — SHA-256 becomes a hash impl + a matrix
   row, never a rewrite.
3. **`rit-sdk` as the single stable seam** — rithub and tooling depend only on
   the SDK, insulating them from inner-crate churn.
4. **Compat gate is a release blocker, not a wish** — `rit-testkit` +
   `rit-fuzz` + fixtures are Stage 0, and the forge cannot begin until the core
   gate is green.
5. **No payment-rail coupling** — `rit` stays a systems tool; provenance ties are
   additive and owned by consumers (Mint/Passport), preserving the security
   contract.
