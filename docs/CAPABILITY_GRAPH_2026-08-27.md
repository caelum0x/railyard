# Capability Graph — Provenance, Payments, Recovery, and Authoring

_State captured 2026-08-27 and updated through 2026-08-28. This is an
implementation map, not a deployment or credential plan._

## The product graph now

```text
Identity Lock Passport(version, digest, owner)
        │ SELECTED/INSPECTED/EXPORTED by web + mobile + desktop
        │ RESOLVES_FOR {provider, modality=video}
        ▼
Menivor MediaJob(request_id) ─SUBMITS→ durable Prism job(request_id)
        │                                  │
        │                                  └─POLLS/RECOVERS→ Provider output
        │ RESERVES                                      │
        ▼                                               ▼
Mint Hold ─CAPTURED_AS──────────────→ signed Receipt(request_id)
        │                         │
        └──────── ATTESTS ────────┘
                    │
                    ▼
Identity Lock GenerationRecord(passport digest, output, request, payer, proof)

Menivor API ─────────────┐
                        ├─NORMALIZES THROUGH→ provider-adapter-kit execution/v1
Standalone Prism ───────┘                         │
        │ owner + correlation intent              ├─VALIDATES→ handle/state/output
        │ durable provider handle                 ├─CLASSIFIES→ error + acceptance
        └─lost response RECOVERS BY CORRELATION   └─BILLS→ success-only exact/estimate
                                                     │
                          public MediaProviderExecutor ─DISPATCHES→ provider transport
                                                     │ one POST after acceptance ambiguity
                                                     └─POLLS→ terminal output or typed failure

Menivor MediaJob ─PROJECTS→ Canonical Generation View
       │                         │ status/error/cost/settlement
       │                         │ correlation/provider request/executor
       │                         │ identity/signed evidence (nullable, never invented)
       │                         └─RESUMED_BY→ web + mobile + desktop
       │
Desktop Local rf-runtime Job ─PROJECTS──┘  executor=desktop_local, cost=0,
       │                                   no synthetic Mint receipt
       ├─STAGES→ immutable inputs/<sha256> ─EXECUTES→ ffmpeg-local-v1
       │                                                │ atomic publish
       │                                                ▼
       └─PERSISTS→ atomic local snapshot ←VERIFIES─ outputs/<task>.mp4
                    (fingerprint, attempt, exact lease, SHA-256 artifact)
                                      │
                restart ─REOPENS/RECONCILES─→ verified bytes stay terminal;
                  missing/corrupt bytes retry exactly once, then exhaust recovery

Actor ─MEMBER_OF→ personal/workspace principal ─OWNS→ Menivor wallet source
       │                         │                         │ maps privately to Mint
       │                         └─RBAC(wallet.transfer)──┤
       ├─SELECTS source ─RESOLVES→ identity-bound recipient
       │                         │ saved shortcut follows user/account, not handle
       └─REVIEWS→ one-use challenge(actor, source, recipient, amount, key, expiry)
                                      │ consumes with durable pending command
                                      │ stable operation id; account-locked limits
                                      ▼
                     idempotent Mint transfer ─EMITS→ signed sender Receipt
                                  │                         └─BINDS→ privacy-safe
                                  │                           Ed25519 Attestation
                                  │                                  │ pinned verify
                                  └─PROJECTS→ safe activity ←────────┘
                                           │
                     leased beat ─REPLAYS→ pending command exactly once locally
                                           │
                                sender-only verified proof/status
                         Mint remains the only balance ledger

Recipient ─SHARES→ Mint `ait1…` receive address ─RESOLVES_ONCE→ Mint account
       │                         routing only; never debit authority        │
       └──────────── RECEIVES exact AI-token transfer ←────────────────────┘
                              receipt + attestation bind resolved account

Agent ─AUTHORIZES→ Relay(owner principal, request id)
                           │ PRECOMMITS exact reserve command
                           ▼
                Postgres billing outbox ─RESERVES→ Mint Hold(hold id)
                           │ durable hold before provider I/O
                           ▼
                  TensorZero ─ROUTES/STREAMS→ Provider
                           │ success + normalized usage
                           │ PRECOMMITS one terminal direction
                           ▼
         capture/release command ─REPLAYS EXACTLY→ Mint
                           │ receipt + signed hold attestation
                           │ pinned-key verification
                           ▼
       Postgres evidence(owner, request) ←ATOMIC WITH─ command completion
                           │
             agent-runtime ←LOOKS_UP/VALIDATES─┘

Paid batch start ─PERSISTS→ BatchPending(owner, batch id, model, hold)
                                  │ authenticated aggregate poll + single lease
                                  ├─PENDING→ retain hold / yield poll lease
                                  ├─FAILED─→ immutable release
                                  └─COMPLETED(Σ usage)→ immutable exact capture

RuntimeAgent ─PROMPTS→ Rig RelayModel ─SENDS→ TensorZero selector
       │                    │                    │ canonical direct model
       │                    └─402 ONCE───────────┘ prices hold + exact usage
       │                         │ policy checks payee/resource/AIT/hold/max/expiry
       │                         └─SIGNS + RETRIES IDENTICAL BODY→ Relay
       │
       ├─STREAMS→ incremental SSE ─REQUIRES→ terminal usage + owner-scoped receipt
                         │ disconnect/error/missing proof fails without resubmit
                         └─EXPOSES→ bounded PaymentEvidence(owner ≠ payer allowed)
       └─NEVER DIRECT-SETTLES→ Mint (Relay remains settlement owner)

Turnstile Challenge ─OWNED_BY→ exact persisted nonce record
Abacus Request ─RESOLVES→ shared Postgres Policy
        │
        └─RESERVES→ persisted open Mint Hold ─LEASED/RECOVERED_BY→ one replica
                                                     │
                                      signed Receipt Projection (Postgres)

Worktree file ─HASHES_TO→ Git Blob ─STAGED_IN→ rit Index
                                     │
                         index.lock + fsync + atomic rename
                                     │
                    recursive canonical write-tree
                                     │
                                     ▼
                 Commit object ─CAS→ current branch ref
                                     │
Tree/commit snapshot ─PREFLIGHTS ALL COLLISIONS─┬─ATOMICALLY REPLACES→ worktree
                                               └─SYNCHRONIZES→ Git index
Current index ─EXPLICIT RESTORE──────────────────────────────→ selected paths

receive-pack pkt-lines ─PARSE/BOUND→ ref command + raw pack boundary
       full/thin pack ─GITOXIDE VERIFY/EXPAND→ scratch pack + index
                     │ verified target/quarantine ODB bases only
                     ▼
          decoded objects ─REHASH/ISOLATE→ quarantine ODB
                                  │ declared OID + size/count bounds
                                  ▼
                         strict connectivity/fsck
                     tree/type/tag/commit edges + graph bounds
                                  │ fast-forward policy
                                  ▼
                 immutable object publish ─THEN→ one branch ref CAS
                     failure: quarantine removed, ref never moves
```

## Implemented ownership and failure boundaries

| Artifact | Single authoritative owner | Durable key | Retry behavior |
|---|---|---|---|
| Menivor identity binding | Menivor `media_jobs` | media job id + passport/version/digest | Capture receipt is committed before remote attestation; retry reuses proof and never captures twice |
| Identity provenance | Identity Lock | deterministic `gen_ext_{request_id}` | Exact replay is a no-op; different payload for the same id is a conflict |
| Relay inference settlement | Relay Postgres outbox; Mint remains the balance/hold authority | `(owner_principal, request_id)` plus immutable payer/payee/held amount/hold/idempotency inputs and an execution-owner token; paid batches additionally bind a globally unique batch id and billing model | Reserve is committed before Mint, hold id before TensorZero, and capture/release before its Mint call. Lease renewal and fenced transitions prevent stale workers from mutating a live execution; `SKIP LOCKED` recovery only replays the persisted Mint command and never reruns inference. `batch_pending` is not an orphaned HTTP request: one authenticated aggregate poller retains/releases/captures it according to provider state. Capture and evidence complete in one transaction |
| Relay settlement evidence | Relay Postgres | `(owner_principal, request_id)` | Pinned-key verified receipt + full hold attestation replay is a no-op; cross-principal lookup is indistinguishable from not-found. A funding payer may differ from the evidence owner |
| Streaming proof consumption | `aitp-client` + Rig Relay backend | request id + authenticated evidence owner; funding payer remains separate | Incremental bytes remain live; clean EOF requires trustworthy terminal usage and an owner-scoped receipt whose full Mint attestation verifies against a caller-pinned key. Early drop transfers the lookup obligation to a bounded reconciler; unresolved ids remain explicitly retryable. No proof path resubmits inference |
| Turnstile challenge | shared challenge store | nonce | Authorization must equal the exact issued resource/payee/amount/scheme/expiry; consume is atomic |
| Abacus open-hold recovery | Mint for money, Abacus shared store for recovery | hold id | Any replica may retry idempotent Mint release; local JSON remains an explicit single-replica fallback |
| Abacus policy/receipts | Abacus Postgres projection; Mint remains money authority | agent id / hold id / request id | Declarative policy import is transactional; identical receipt replay is a no-op and conflicting replay fails closed |
| Prism provider job | Prism durable owner-scoped store | generation id + request correlation id | Restart resumes the provider handle; transient/missing lookups retain the Mint hold, explicit terminal failure releases it |
| Provider execution contract | versioned `provider-adapter-kit`; provider HTTP clients behind public `MediaProviderExecutor` | provider + immutable model ref + run id + schema version | Submit failover is allowed only when work is known not accepted. Ambiguous POST outcomes never resubmit; correlation recovery adopts an already accepted Prism handle. Poll states are monotonic and terminal failures are non-billable |
| Menivor generation view | Menivor `media_jobs`; local desktop runtime for local jobs | generation id + correlation id | Detail/list reopen accepted work and poll existing handles; no client resubmits. Failed jobs expose zero effective cost; local releases immediately while Mint stays held until verified release evidence is durable |
| Menivor paid media settlement | Mint ledger plus Menivor's durable rail/intent/evidence projection | media job id + exact operation idempotency key | Reserve/capture/release intent is committed before each Mint POST. A terminal public state requires Mint's receipt and a pinned-key-verified `mint-hold-settlement-attestation-v1`; ambiguous work replays only the same operation and never changes rail or terminal direction |
| Menivor AI-token transfer | Mint balance ledger; Menivor owns typed source edges, RBAC, bounded review, durable command, pinned verification, and safe activity projection | source account + client idempotency key + one-use challenge; stable Mint operation | Exact durable retry uses persisted source/recipient/amount even after review expiry or authorization change. Ambiguous failures stay pending and are leased/replayed automatically; definitive non-mutating Mint 4xx responses become terminal `rejected`; completion requires a valid privacy-safe attestation and projects activity once |
| Mint public receive address | Mint address index routes an `ait1…` destination to one authoritative recipient account; Menivor remains responsible for sender authorization | resolved recipient account is frozen into Mint's idempotent ledger command | Exact replay resolves to the same persisted command/evidence; malformed addresses fail before money movement, unknown valid addresses are 404, and changed-body reuse is 409 |
| Desktop local render execution | Menivor desktop atomic local snapshot plus app-confined input/output artifact root | task id + versioned SHA-256 execution fingerprint + attempt id + lease token + output digest | Completion requires matching non-empty output bytes. Exact live leases survive restart; stale callbacks fail closed; missing/corrupt verified output creates exactly one retry lineage, then becomes `recovery_exhausted` |
| Git index | `.git/index` | `.git/index.lock` | A competing/crashed lock fails closed; published index is checksumed, fsynced, then atomically renamed |
| Git branch update | loose ref under `.git/refs` | expected parent oid | Commit/tree objects publish first; branch moves only through compare-and-swap, leaving a safe unreachable object on conflict |
| Git pack receive | repository-owned scratch + quarantine ODB; target ODB remains authoritative only after verification | pack checksum/index + decoded object ids + proposed ref/expected old id | Strict pkt-line parsing identifies the pack boundary. gitoxide validates full/thin packs and resolves thin bases only through verified target/quarantine ODB reads. Re-hashed decoded objects stay isolated until bounded connectivity/fsck and fast-forward checks pass; connected immutable objects publish before one ref CAS. Expected-old loose-ref deletion is locked and fsynced; packed backing values fail closed. Ref advertisement + stateful process transport are proven; multi-ref atomicity and Smart-HTTP/SSH listeners are not yet claimed |
| Git snapshot materialization | `rit` worktree + index transaction | target tree oid | All modified/untracked/symlink collisions are rejected before mutation; files publish through temp+fsync+rename; `HEAD` is deliberately unchanged |

## Current honest capability boundaries

- Menivor identity binding is implemented for video. Image/audio identity
  conditioning is not claimed.
- Menivor web, mobile, and desktop share the identity/proof response contract;
  native mobile compilation remains deferred until the September 1 Expo window.
- Relay remains an Apache-2.0 TensorZero fork for routing, streaming,
  observability and provider integrations. Railyard's original overlay now owns
  a restart-durable Postgres settlement state machine: process memory is not a
  money authority, terminal direction is immutable, and recovery never invokes
  TensorZero. Production billing also requires TensorZero authentication and
  operator-pinned Mint Ed25519 verification keys.
- Relay's adaptive provider-health scorer is also restart-aware: Postgres
  checkpoints retain latency/error EWMAs and circuit state, restore before
  traffic, subtract downtime from breaker cooldowns, and ignore malformed rows.
  This is explicitly best-effort routing optimization, not settlement authority.
- Paid TensorZero batches retain one durable hold across start and aggregate
  polls. Partial per-inference polling is unavailable under billing because it
  cannot reconcile the whole hold from partial usage.
- TensorZero application-level SSE error frames and body errors are sticky
  failures. A later `[DONE]` or clean EOF cannot convert them into a charge.
  A non-streaming success without trustworthy usage is withheld as a billing
  error and released, rather than delivered as free inference.
- API video/audio submit, poll, detail and timeline now return one generation
  superset. Reopening a pending detail polls its existing provider handle; a
  transient lookup leaves durable state and the hold inspectable for recovery.
- Paid image/video/audio jobs stamp `mint` or `local` before provider I/O. Mint
  rows persist `reserve_pending`, `held`, `capture_pending`/`release_pending`,
  and only become captured/released after the exact signed command attestation
  verifies against a configured pinned Ed25519 key. Flat receipt fields or a
  terminal database flag alone project as held/no-evidence. Release evidence
  binds the positive amount returned from held to available; it is never labeled
  a charge. Local and desktop-local rows remain unsigned and charge zero.
- The desktop now accepts a user-selected MP4/MOV/WebM, copies the exact bytes into
  an immutable content-addressed `inputs/` namespace, durably records the project,
  fingerprint, attempt and lease, and runs a real 1080x1920 `ffmpeg-local-v1`
  transcode. It atomically publishes under `outputs/` and reaches completion only
  after `rf-runtime` verifies the non-empty bytes, SHA-256 digest and execution
  fingerprint. This is an honest local transcode path, not prompt-to-video AI
  generation, and it remains a single-coordinator local scheduler.
- Restart re-verifies completed local bytes. Missing/corrupt output creates one
  parent-linked retry; a second failure becomes `recovery_exhausted` without an
  attempt loop. Every progress/failure/completion callback is fenced by the exact
  attempt id and lease token. Canonical history exposes safe relative artifact
  references and lease metadata but never tokens, absolute paths, `file://` URLs,
  cost, margin, or synthetic Mint evidence.
- Verified-only OS preview re-verifies immediately before returning the confined
  path. A same-user process could still mutate a file between that check and an
  external viewer's read; that is an explicit local operating-system trust
  boundary, not a cryptographic guarantee across processes. Local rendering also
  requires `ffmpeg` to be installed.
- AI-token transfer recovery is automatic: a 30-second beat claims one pending
  command just in time with a fenced lease, replays the persisted Mint operation,
  verifies its attestation, and finalizes through the same row-locked service path.
  Initial Mint account provisioning can still orphan an unused account if its
  response is lost; that failure cannot debit funds.
- Personal and workspace wallets are now explicit selectable sources. Workspace
  owner/admin roles may transfer; other members fail before Mint. Agent and
  service ownership edges are representable but deliberately non-selectable until
  a delegation model exists. The public contract exposes opaque Menivor source ids,
  labels, roles and limits, never Mint ids or internal principal references.
- A transfer review is a ten-minute one-use server challenge bound to the actor,
  source account, immutable recipient user/account, displayed identity snapshot,
  amount and idempotency key. Changing any reviewed input fails closed. Once its
  durable command exists, an exact retry is allowed even if the challenge expires
  or workspace membership changes; reconciliation never re-resolves mutable auth
  or handles.
- Per-transfer and UTC-day limits are Menivor authorization policy, not another
  money ledger. Pending and completed commands count toward the daily total under
  the source-account lock; rejected commands do not. Saved-recipient shortcuts are
  owner/source scoped and follow immutable recipient identity across handle changes.
- Web, mobile and desktop persist the reviewed source, recipient, amount,
  challenge and idempotency intent before sending. Legacy pending records are
  recoverable only as known personal-wallet intents and are never silently applied
  to a workspace source. Native Expo and full GPUI application builds remain
  outside this verification wave.
- Every newly completed transfer is verified against a configured pinned Mint
  Ed25519 key. The sender receives exact signed privacy-safe claims; raw Mint
  account ids are absent. This proves one transfer result, not the complete Mint
  hash chain, global conservation, public-chain anchoring, or public-handle binding.
- A public `ait1…` address is a shareable receive route, not proof of identity or
  permission to debit. Mint resolves it before constructing the ledger command;
  signed receipts and privacy-safe attestations bind the resolved account. The
  transfer sender remains an `acct_…` selected behind Menivor ownership/RBAC.
- Relay lookup closes receipt delivery after streaming, but it does not expose
  another principal's proof.
- High-level buffered and streaming `RuntimeAgent` calls can answer one Relay-issued
  AITP `402` through Rig's custom HTTP backend. The signer is policy-bound to the
  pinned payee, exact configured chat/model resource, `AIT`, hold scheme,
  positive maximum and live challenge. A second `402` is terminal. The separate
  `PaidCompletionModel` remains a fixed-price direct compatibility path and must
  never wrap a Relay-billed model.
- Direct TensorZero model selectors are canonicalized into the authoritative
  Relay price-catalog identity for both the pre-inference hold and response
  usage. Named functions still expose only function/variant, not the concrete
  routed model; they use the unknown-model default. The built-in default is now
  the maximum embedded rate ($15/$75 per million), but an operator-supplied
  default remains an explicit pricing-policy boundary.
- The AITP-aware Rig backend preserves genuine paid SSE byte streaming. It does
  not report clean EOF until terminal usage is complete and Relay's authenticated
  owner-scoped evidence route returns a signed receipt plus the exact Mint
  attestation. The client independently checks its domain-separated digest,
  caller-pinned Ed25519 signer, strict claims, and receipt sequence/operation/
  operation-hash/amount/hash binding. Rig multipart forms encode once and replay
  identical bytes through the same one-challenge path; route availability and
  resource-policy scope remain explicit configuration.
- Turnstile and Abacus are replica-safe only when their Postgres URL is set.
  Their in-memory/local-file modes deliberately identify themselves as
  single-replica.
- `rit add` accepts recursive directories, nested ignore rules, symlinks, and
  tracked deletions; write-tree/commit/checkout-tree/restore are real-Git
  interoperable. File↔directory type transitions remain conservatively rejected.
- `rithub` remains gated. Full/thin pack ingestion, decoded-object quarantine,
  connectivity/fsck, fast-forward enforcement, one-ref CAS, and a stock-Git
  stateful receive-pack process path now exist. Clone/fetch works through an
  explicit stock-Git upload-pack compatibility bridge, but native Rust fetch,
  Smart-HTTP/SSH listener integration, multi-ref atomic receive, merge,
  packed-refs rewriting/deletion, interrupted-ingest recovery, and broader
  hostile-input fault tests must land before it hosts repositories.
- API and Prism now consume the same versioned provider request, handle, result,
  output, error and cost vocabulary. Prism keeps credential mode beside the
  canonical provider handle, never inside it, and never persists a BYOK secret.
  A caller must resend the matching BYOK credential while polling.
- Provider submit timeouts, connection failures, HTTP 408/5xx responses and
  successful responses without a durable run id are acceptance-ambiguous. They
  retain the job and wallet hold and cannot trigger a second POST or provider
  fallback. Prism can recover a bound job by owner-scoped correlation id; a truly
  handleless upstream outcome remains an inspectable unresolved intent requiring
  reconciliation rather than unsafe resubmission.
- `cost_micros` is success-only and preserves an explicit provider-reported zero.
  For providers that do not report usage, the successful value is still the
  versioned catalog estimate recorded in the handle; it is not claimed as a
  provider invoice. Failed/cancelled work is always zero-cost.
- The execution DTO package is independently installable. Concrete provider HTTP
  clients still live behind the API's public executor module, which standalone
  Prism packages at runtime; moving those clients into a second installable
  package would change physical ownership, not the now-shared behavior contract.

## Next larger goals (ordered by dependency)

### G1 — Complete the provenance product surface — DONE

Expose passport selection and proof inspection as first-class web/mobile/desktop
flows, add image/audio conditioning only where providers have a real portable
binding, and make provenance export/verifier endpoints consume the same signed
receipt vocabulary. Do not synthesize proof.

### G2 — Make settlement evidence the portfolio-wide request spine — MEDIA SPINE DONE

Give Menivor, Vend, Turnstile, and Abacus one request/evidence lookup contract;
propagate correlation ids through provider, capture, provenance, and margin
events; add recovery views for captured-but-not-yet-projected work.

### G3 — Finish shared control-plane state — ABACUS SLICE DONE

Abacus policy/receipt projections now move from local files into shared
transactional storage and its recovery loop has explicit leases plus
deterministic replica tests. Persisting Turnstile tool/provider registration and
giving its recovery loops the same lease vocabulary remain later graph work.
Mint stays the sole money authority.

### G4 — Finish `rit` local authoring before touching `rithub` — SAFE LOCAL SLICE DONE

Recursive add + ignore rules, write-tree, commit with atomic ref CAS, safe
checkout-tree, and explicit restore are complete. G9 now supplies the shared
packet/receive-command vocabulary, gitoxide-backed full/thin pack ingestion, and
decoded-object quarantine core. A stateful receive-pack process now advertises
refs/capabilities and accepts stock-Git create/fast-forward pushes. Upload-pack/
fetch is functional through an argument-safe installed-Git bridge; replacing
that bridge with native Rust protocol code, listener integration, multi-ref
atomic receive, packed-refs rewriting, and merge remain the gates that unlock
the Rust forge; `rithub` continues to own collaboration metadata, never Git
internals.

### G5 — Converge Menivor's five applications on one media execution graph — RESUMABLE VIEW DONE

API submit/poll/detail/timeline, web, mobile, desktop, Prism-backed handles, and
desktop-local jobs now converge on one resumable view. First-party surfaces show
the same status, terminal error, cost, settlement, identity, correlation,
executor, provider request, and signed-evidence fields. Provider-adapter package
extraction remains the next implementation depth; verified durable local desktop
state is now implemented in G6/G13.

### G6 — Make local desktop execution restart-durable — VERIFIED LOCAL SLICE DONE

Local envelopes, exact leases, attempts, fingerprints and verified artifact
records persist. The desktop now stages selected video bytes, submits durable work,
executes a real portrait ffmpeg transcode, atomically publishes the output and
verifies its digest before completion. Exact live leases restore; expired live
work requeues safely; missing/corrupt completed bytes create exactly one recovery
attempt before explicit exhaustion. This proves end-user local transcoding, not
local AI generation or a distributed scheduler.

### G7 — Extract one provider-adapter core for API and Prism — DONE

`provider-adapter-kit` v0.2 now owns strict execution/v1 request, handle, result,
output, error and success-only cost schemas plus registry, state-regression,
output-safety and failover rules. Menivor API and standalone Prism both execute
through the public `MediaProviderExecutor`; Prism has no imports of API-private
gateway helpers. The API persists the versioned result with legacy dual-read,
while Prism persists the identical canonical handle under owner + correlation
idempotency and keeps terminal state absorbing across concurrent polls.

Serialized parity fixtures replay queued, running, completed-at-zero-cost and
failed-without-charge results through both surfaces; unknown states, empty
completion and billable failure are rejected before persistence. Lost Prism POST
responses recover by correlation without submitting twice. Provider transports
perform one submit POST once acceptance becomes ambiguous, while ordinary GET
polls retain bounded retry behavior. Local evidence: 73 shared-core tests, 26
Prism/parity tests, 62 API gateway/recovery tests and 80 concrete provider
transport tests, plus the TypeScript package build and Alembic head check.

### G8 — Give every paid media job signed settlement evidence — DONE

Ordinary image/video/audio and Identity-Lock video now use the same stamped rail,
durable hold and exact replayable terminal intent. Mint capture/release responses
carry a privacy-safe Ed25519 attestation over operation hash, amount, receipt,
reference/hold/party commitments and signer key. Menivor recomputes the canonical
Mint operation hash, verifies the exact envelope against pinned keys, persists it,
and only then exposes `verified: true` evidence through detail/list/timeline,
provenance and evidence export. Web, mobile and desktop distinguish captured,
released, held and unsigned local work without inferring proof from status. A
definitive local provider rejection commits its release before the endpoint
re-raises, closing the stranded-reservation rollback window found during testing.

### G9 — Implement `rit` protocol v2 and quarantined receive — SAFE RECEIVE CORE DONE

Binary-safe pkt-line decoding/encoding now enforces Git's 65,516-byte payload
limit plus aggregate packet/byte limits and understands flush, delimiter and
response-end controls. Receive-pack parsing accepts create/update/delete command
vocabulary, validates ref names and requested capabilities against the server's
advertisement, detects the raw pack boundary, and emits standard report-status.
It intentionally follows Git's documented pack-protocol push boundary rather
than inventing a proprietary protocol-v2 push command.

`ReceiveTransaction::stage_pack` now feeds full and thin transport packs through
gitoxide's checksum, index, REF_DELTA and OFS_DELTA machinery inside a temporary
scratch boundary. Thin bases are available only through Rit ODB reads that
verify their object identity. Every expanded object is independently re-hashed
into a second scratch ODB; only a completely valid batch enters the publishable
quarantine. Declared OIDs, raw/decoded byte and object budgets, strict tree
modes/names/order, type-correct tree/tag/commit edges, complete reachability,
graph bounds, disconnected-object refusal, branch tip type, and fast-forward
policy all pass before publication. Objects publish idempotently, then exactly
one ref moves by existing CAS; a race can leave only safe unreachable immutable
objects. Stock-Git full and genuinely thin packs promoted into bare repos pass
`git fsck --strict`; checksum corruption, truncation, and missing thin bases
publish nothing.

The transport-agnostic executor composes command parsing, pack staging,
publication, and privacy-safe report-status. The `rit receive-pack <git-dir>`
process adds protocol-v0 ref advertisement and bounded stdin/stdout serving.
Stock `git push` creates and fast-forwards through this process, while a real
`git push --force` is rejected by server policy and leaves the branch unchanged.
The server advertises `delete-refs` and accepts a stock-Git deletion only when
the exact old OID is stored as a loose ref. The delete path takes the same
per-ref lock, compares current state, unlinks, fsyncs the parent, and cleans its
lock. A packed backing value is rejected before mutation so removing a loose
shadow can never reveal stale history. Stock-Git success and packed-ref refusal
are both covered.
The companion `rit upload-pack <git-dir>` validates/canonicalizes the hosted bare
repository and delegates stdin/stdout to `git upload-pack --strict` without a
shell. Stock Git clone and incremental fetch pass through this compatibility
bridge. It is deliberately recorded as an installed-Git dependency, not a
native Rit fetch implementation.

G9 is not complete: native Rust upload-pack/fetch negotiation, Smart-HTTP/SSH
listener integration, multi-ref atomic transactions (including mixed
deletions), packed-refs rewrite/deletion, interrupted-ingest recovery/fault
injection, and merge remain. `rithub` stays gated on those items.

### G10 — Harden AI-token transfer recovery and trust UX — PORTABLE RECOVERY DONE

The handle-resolve/review/idempotent-send flow works across API, web, mobile, and
desktop. Menivor now precommits a stable command, persists Mint's complete signed
sender receipt, and exposes sender-only transfer lookup; all three clients can
recover an existing transfer without issuing a new logical send. Configurable
limits and stronger recipient identity remain next.

### G11 — Automate transfer reconciliation and cryptographic verification — DONE

Pending commands are claimed just in time with PostgreSQL `SKIP LOCKED` and a
fenced owner update (conditional-update fallback on SQLite), replay their persisted
Mint operation, and project the result once. For the separate privacy-safe
`mint-transfer-attestation-v1`, Mint signs a domain-separated SHA-256 digest over
the exact transmitted claims bytes, including operation/receipt hashes, amount,
sequence, timestamp, opaque-reference hash, and per-transfer unlinkable party
commitments. Menivor verifies it against a pinned Ed25519 key before completion;
the public key document is discovery for independent verifiers, never dynamic
settlement trust. Client language may say "verified" only after this succeeds.
Definitive non-mutating Mint rejections are terminal so a later top-up or account
state change cannot silently execute an old failed send; ambiguous network,
rate-limit, server, attestation, and result-persistence failures remain retryable.

### G12 — Expand the token account graph without creating a second ledger — DONE

Model personal, workspace, agent, and service accounts as Menivor ownership edges
over Mint account ids. Add explicit source-account selection, workspace RBAC,
saved recipients, per-transfer/daily limits, and a review challenge bound to
source, recipient identity, amount, and expiry. Mint continues to own balances;
Menivor owns authorization and projections. Personal/workspace selection, dedicated
owner/admin transfer RBAC, identity-bound saved recipients, locked limits, one-use
review binding, durable exact retry, and cross-client pending-intent parity are now
implemented. Public `ait1…` routes now make personal and workspace accounts valid
typed destinations without exposing Mint account ids. Agent/service edges
intentionally stop at representation; delegated sending remains a future capability.

### G13 — Ship durable local execution through the desktop app — DONE

GPUI accepts a real local source video, stages immutable content-addressed input,
persists project/task/lease state before execution, drives `rf-encode`'s ffmpeg
command with live heartbeats, and atomically publishes verified output. Snapshot
schema v3 binds SHA-256 output bytes to the execution fingerprint and records
attempt, lease, retry parent/reason and retained artifact history. Recovery
re-verifies bytes and creates exactly one retry lineage for missing/corrupt output;
stale attempt or lease callbacks fail closed. The additive canonical
`execution.local` projection exposes this history with portable relative refs and
the existing top-level status vocabulary. Local rows remain zero-cost, released,
unsigned and URL-free.

### G14 — Make TensorZero-to-Mint settlement restart-durable — DONE

Relay now commits one owner-scoped billing row before every Mint reservation,
stores the returned hold id and immutable payee before TensorZero provider I/O,
and commits exactly one capture-or-release payload before its terminal Mint
call. A leased `SKIP LOCKED` worker replays stale reserve and terminal commands
under stable payer-scoped idempotency keys without rerunning inference. Exact
AITP authorization replay returns Mint's original hold, closing the
accepted-before-response crash window. Direct replay uses the persisted resolved
payer and reserve idempotency key even if process configuration later changes.
The execution owner is fenced across hold, inference, heartbeat, and terminal
selection; duplicate callers cannot release a winner's hold. Asynchronous
batches transition to durable `batch_pending` ownership instead of orphan
recovery: only one principal-bound aggregate poller runs at a time, pending
retains the hold, failure releases it, and completion captures summed normalized
usage.

The adaptive provider ranker has its own restart checkpoint boundary. It exports
a lock-consistent snapshot, atomically upserts all observed model/provider rows,
restores validated rows before serving, and checkpoints through TensorZero's
tracked shutdown task set. Open-circuit snapshots store remaining monotonic
cooldown plus wall-clock capture time, so process downtime is subtracted rather
than restarting the full penalty; untrusted durations are capped at 24 hours.
Billing recovery and synchronous guard-drop refunds use the gateway's tracked
task set, with a final reconciliation pass on shutdown; an orderly stop waits
for those durable terminal commands instead of detaching them.

Mint terminal responses include the full signed
`mint-hold-settlement-attestation-v1`. Relay verifies exact payload bytes,
domain-separated digest, pinned Ed25519 signer, receipt hash, operation, amount,
reference, hold, payer and payee commitments before completion. Capture evidence
and command completion share one local transaction; API-key ownership remains
separate from the funding Mint payer. Streaming EOF is pollable and waits for
durable finalization. Terminal usage and `[DONE]` bytes are withheld until the
immutable capture command is committed, active streams renew their lease, and
partial/missing usage fails closed rather than synthesizing a charge. TensorZero
error events remain non-billable even when the wire stream subsequently emits
`[DONE]`. Startup also resolves the advertised AITP address through Mint and
requires it to equal the immutable capture account.

### G15 — Make public Mint addresses usable for person-to-person receipt — DONE

`POST /v1/transfers` now accepts either the historical `acct_…` destination or
a Mint `ait1…` receive address. Address parsing and authoritative lookup happen
before the immutable ledger command; no address can select or authorize the
sender. The resulting signed sender receipt and
`mint-transfer-attestation-v1` name/commit to the resolved recipient account,
not the routing alias. Exact replay returns the same sequence, receipt and
attestation; malformed and unregistered addresses fail before settlement, and
an idempotency key reused with changed money inputs remains a 409 conflict.

The official Rust, Python, and TypeScript SDKs now require the full signed
receipt plus attestation when decoding a transfer response instead of silently
dropping both. The dynamic-language SDKs fail closed when proof identity,
amount, operation, or sequence drifts from the submitted command. Menivor's
production Python client exposes the same
address routes and its live-devserver journey funds Alice, transfers to Bob's
public address, verifies the pinned Ed25519 attestation, and checks replay,
conflict, privacy and exact final balances. That journey runs over a real socket
where loopback binding is permitted; the managed coding sandbox explicitly
skips it because the host denies all loopback binds with `EPERM`.

Menivor also normalizes Mint's real Rust receipt `op_hash` wire form (a JSON
array of exactly 32 bytes) to canonical lowercase hex for attestation comparison
and sender-facing projection, while preserving the untouched signed receipt.
The older hex form remains compatible; wrong lengths, booleans, out-of-range
bytes, uppercase hex and all other representations fail closed as pending and
cannot create activity.

All three Menivor clients bind review and confirmation to the selected source's
Mint-authoritative balance. Web, mobile and desktop fetch `GET /wallet` with the
opaque `source_account_id`; mobile/desktop refuse a known over-balance draft,
discard late balance responses from a previously selected source, and refresh
the exact source after completion. A workspace transfer updates only that
workspace projection and can never overwrite the personal media-wallet balance.

### G16 — Connect high-level Rig agents to Relay's dynamic AITP billing — DONE

Relay now canonicalizes real OpenAI-compatible TensorZero selectors such as
`tensorzero::model_name::openai::gpt-4o` to the same
`openrouter/openai/gpt-4o` identity used by its embedded and operator price
catalogs. Request holds and non-streaming/SSE response usage share that helper;
TensorZero selector aliases in operator overrides normalize to the same key and
duplicate aliases fail startup validation. Nested shorthand tails remain intact.
Unresolved function selectors stay on the unknown price rather than guessing a
routed model; the default was raised to the maximum embedded input/output rates
so an embedded Opus route cannot be under-held by default.

`relay-provider` now supplies a local Rig `HttpClientExt` newtype over the
existing `PaidHttpClient`. `RelayModel::connect_paid` installs it below Rig's
OpenAI Chat Completions model. A funded request is sent once; a valid `402` is
signed and the byte-identical body retried once with correlated AITP headers; a
second `402` stops. Controlled AITP retry headers replace any stale caller
values, so exactly one version, request id and authorization reaches Relay. The
signer refuses a wrong payee, currency, scheme, amount, resource/request scope,
expiry or empty nonce. This path never invokes `PaymentGateway` or
`PaidCompletionModel`, so Relay remains the single owner of Mint reservation,
exact token settlement and evidence. The live agent example now uses this path
and no longer accepts direct Mint server credentials or a second local price.

### G17 — Preserve live paid SSE through the high-level Rig model — DONE

`HttpTransport` now has a genuine incremental response contract; its production
Reqwest implementation forwards `bytes_stream()` with backpressure rather than
buffering the model response. `PaidHttpClient::send_streaming` resolves the 402
challenge before exposing body bytes and retries only once with the identical
request. The Rig backend validates SSE events across arbitrary chunk boundaries,
keeps application errors sticky, requires internally consistent terminal usage,
and performs the owner-scoped settlement lookup before allowing clean EOF.

Successful proof is retained in a bounded queue exposed by
`RelayModel::drain_payment_evidence`. Evidence records preserve the authenticated
Relay owner separately from the account that funded the challenge. A disconnect,
missing usage, missing receipt, malformed terminal event or second 402 reaches
the stream consumer as an error and never triggers another inference submission.
`RuntimeAgent::stream` exposes this through Rig's high-level agent loop.

### G18 — Close client proof, cancellation, and multipart boundaries — DONE

Relay evidence is no longer accepted on authenticated projection alone. The
client requires the full `mint-hold-settlement-attestation-v1`, verifies its
exact transmitted payload digest and Ed25519 signature against a caller-pinned
Mint key, validates strict claims, and binds sequence, operation, operation
hash, amount, and receipt hash back to Relay's projected receipt. Descriptive
evidence receipt fields and terse Mint header aliases decode into one type;
missing or tampered attestation fails closed and remains pending, never evidence.

Every successful paid stream registers its request id before exposing body
consumption. Clean EOF reconciles synchronously. Dropping the consumer stream
before EOF transfers that obligation to a bounded Tokio task, while environments
without a runtime retain the id for explicit `reconcile_payment_evidence` during
shutdown/recovery. Neither path resubmits inference. Rig multipart forms now
encode once, set their fixed boundary, and replay byte-identical bytes through
the existing one-402 authorization retry.

### G19 — Send reviewed AI tokens to public account addresses — DONE

Menivor now accepts either a public person handle or an exact Mint `ait1…`
address throughout recipient resolution, challenge review, durable command
creation, recovery, and sender-safe result projection. Mint's authoritative
address lookup resolves the route to an existing local account edge before any
command is committed. Personal and workspace owners are snapshotted as typed
`kind/ref/label` identity; workspace destinations carry no fabricated user id.

Challenge binding version 2 seals the destination kind, owner reference,
account, public route, label, actor, source, amount, idempotency key, and expiry.
Version-1 challenges retain byte-identical legacy verification. Handles remain
case-insensitive while Mint addresses compare exactly. Settlement revalidates
the persisted owner edge, applies Mint rollout to the destination principal,
and uses the same pending-command, pinned-attestation, activity-once, and exact
retry machinery as handle transfers. Web, Expo, and GPUI render addresses
without a false `@` prefix and do not offer person-shortcut saving for an
address. Internal `acct_…` values never enter recipient, review, activity, or
status DTOs.

### G20 — Make external long-video clipping produce durable shorts — DONE

Menivor's MIT-derived clipping pipeline now accepts an external public HTTPS
video as a real source instead of returning 501. The worker resolves and pins a
public address through the existing SSRF guard, disables environment proxies,
streams at most 1 GiB into an atomic temporary file, accepts only video/binary
media types, and feeds FFmpeg only local bytes. Every redirect is independently
normalized, DNS-vetted, and pinned; loopback, private, link-local, metadata,
reserved, credential-bearing, fragment-bearing, malformed, empty, oversized,
and non-video sources fail before media processing. The webhook operator's
internal-network override cannot weaken this user-input boundary.

Each rendered short and optional stitched reel is uploaded to its deterministic
R2 job key before the scoped work directory is removed. Persisted clip views
contain durable result keys rather than dead local paths, and promotion uses the
already-rendered clip from time zero through its exact duration instead of
re-fetching the original external URL. Stable source metadata uses a SHA-256
reference, so signed query parameters do not leak into clip results.

### G21 — Bind wallet funding to the selected payment provider — DONE

Menivor wallet checkout now declares exactly two credential-gated hosted rails,
Creem and Stripe. The request type, router dispatch, durable payment intent,
provider checkout adapter, generated OpenAPI contract, and first-party client
types agree on that set. Selecting Stripe cannot silently create a Creem session.
The hosted URL and checkout id are persisted so an exact idempotent retry can
resume checkout; reusing the key with a changed amount or provider fails closed.
Wallet amounts and disclosed fees are whole USD cents, eliminating silent
provider-side rounding of the durable quote.

Every verified webhook is normalized behind one provider-bound processor. A
grant requires exact agreement on payment provider, intent id, hosted checkout
reference, currency, charged total, credited amount, and—when Mint-backed—the
Mint account echoed in authenticated metadata. Mint re-confirms the namespaced
`creem:<checkout>` or `stripe:<checkout>` deposit before changing supply, and
both Mint and Menivor retain their existing replay barriers. Unknown provider
paths fail FastAPI validation rather than reaching a placeholder 501.

### G22 — Package rights-clean offline background loops — DONE

Menivor's three formerly unavailable bundled background ids now resolve to real
silent 720×1280 H.264 MP4s shipped inside the API worker image. The stable legacy
ids preserve stored templates, while public labels and media are original
abstract voxel, neon-runner, and synthwave-road motion rather than trademarked
game capture. A checked-in FFmpeg-lavfi script regenerates every asset without
network access, model output, or third-party footage.

Materialization prefers the packaged asset and therefore needs neither a stock
API key nor R2. Paths are resolved under one immutable asset root, traversal and
missing/empty files fail explicitly, and the existing stock and operator-owned
R2 paths remain available for future catalog entries. The normal compose stage
still copies the selected loop into the job's owned R2 prefix before rendering.

### G23 — Carry one Menivor identity across native packages — DONE

The locked Menivor brand now owns a canonical play-hook-spark application mark
in its coral, orange, violet, ink, and warm-paper palette. Expo references a
1024px opaque iOS/root icon, clean transparent Android adaptive foreground,
transparent splash mark, and 48px favicon over the same locked ink background.
The native build is still deferred until September 1, but it can no longer fall
back to Expo placeholder artwork.

The same immutable source derives macOS PNG/ICNS, Windows multi-resolution ICO,
and Linux PNG output. macOS bundle metadata/script, the freedesktop entry, and
AppImage recipe now display Menivor while stable internal executable and bundle
identifiers remain unchanged. Pillow builds the ICNS/ICO containers without the
Xcode icon toolchain; all downstream assets regenerate byte-identically.

### G24 — Make Menivor desktop timeline export operational — DONE

The Rust desktop no longer compiles a disconnected four-panel editor mock. Its
mounted Editor workspace operates on the selected completed reel and assembles a
real ordered non-destructive EDL: up to 200 source spans with per-segment speed
and burned captions, plus an optional positioned text-overlay window. Segment
rows can be added and removed without changing the order sent to the renderer.
Decimal input becomes integer
milliseconds/per-mille without floating-point drift, and `rf-project` now mirrors
the backend's source, minimum-span, speed, text-length, and output-window bounds.

The EDL crosses the authenticated background-worker boundary through an
idempotent `POST /jobs/{source}/timeline-exports`. Exact retries reuse the same
key; changed edits receive a new key. The accepted export becomes a normal
pollable library job, preserving one API-render authority across web, mobile, and
desktop. The 203-line unmounted legacy panel directory was removed rather than
retained as false product surface.

### G25 — Execute the Menivor desktop EDL locally and durably — DONE

The mounted editor now supports arbitrary segment deletion, duplication, and
up/down reordering while preserving the exact visible EDL, plus as many as 50
independently timed and positioned text overlays. Users can send that same
validated EDL to the cloud export contract or select a local source and run it
through the desktop-local durable executor.

The local path stages immutable content-addressed source bytes, persists the
timeline inside the versioned execution input, and binds it to the SHA-256
execution fingerprint. Restart restores the exact timeline; changing a trim,
speed, caption, or overlay under the same task identity fails closed. FFmpeg
trims every ordered source span, applies rational video timing and decomposed
0.25×–4× audio tempo, concatenates audio/video or silent video correctly, burns
one output-clock ASS document, writes to an attempt-private staging file, fsyncs,
and atomically publishes only a digest-verified output.

Caption/overlay burn-in explicitly requires an FFmpeg build with the libass
`ass` filter. Menivor preflights that capability and refuses the render with a
specific error when it is absent; it never silently strips requested text. The
current development FFmpeg lacks libass, so real-process tests cover ordered AV
and silent EDLs while focused Rust tests verify ASS timing and escaping. Release
packaging must carry a libass-enabled FFmpeg/ffprobe pair.

### G26 — Bind desktop timeline edits to an analyzed local source — DONE

The mounted Editor can now select and analyze its local source before render.
The background worker probes the real duration, detects silent inputs, decodes
audio to a bounded 100 Hz mono stream, reduces it to 120 normalized peaks, and
renders those peaks as a waveform. Stale request ids cannot replace the current
selection, and a segment ending beyond the measured duration is rejected before
scheduling.

Analysis hashes the source before and after ffprobe/FFmpeg work. The resulting
SHA-256 is carried into local submission and checked both before and after
content-addressed staging, closing file-replacement races between the displayed
waveform and durable render intent. Rendering reuses the analyzed path without a
second picker, so the user cannot accidentally edit one clip and export another.

### G27 — Ship a signed native Menivor desktop updater — DONE

The Settings workspace now mounts stable and beta update controls backed by the
desktop worker. A build embeds its channel URLs, key id, and Ed25519 public trust
root; builds without that configuration fail explicitly and never fall back to
unsigned updates. Manifests use canonical release bytes, exact semantic versions,
one platform/architecture artifact, HTTPS-only URLs with redirects disabled,
and detached Ed25519 signatures.

After verification, the updater rejects downgrade and persisted high-water
replay, bounds manifests to 256 KiB and artifacts to 1 GiB, checks signed size
and SHA-256, fsyncs and atomically stages the package outside the installation
tree, and rechecks bytes before installer launch. Persisted staging carries the
original signed manifest; restart re-verifies the signature and exact expected
path so locally forged pointer metadata cannot choose an executable. macOS opens
the verified PKG, Windows invokes MSI with argument arrays, and Linux marks and
executes the verified AppImage.

The release-side signer derives size/digest from actual package files, invokes
OpenSSL Ed25519 without serializing the private key, emits the client trust root,
and produces the same canonical payload asserted by Rust. Production URLs and
the public key can be supplied last; the signing private key stays outside the
repository.

### G28 — Bundle the Menivor desktop media toolchain — DONE

All production desktop FFmpeg and ffprobe calls now pass through one resolver.
An absolute development override wins first and fails closed when invalid;
macOS resolves `Contents/Resources/bin`, Windows resolves beside the executable,
Linux resolves `$APPDIR/usr/bin`, and only unbundled developer runs fall back to
`PATH`. Source analysis, audio detection, waveform decoding, local EDL execution,
and output inspection therefore use the same selected binaries.

The worker preflights the resolved pair off the UI thread and reports exact
readiness in the mounted Editor. It requires libass plus every trim, timing,
concat, overlay, scale, crop, and FPS filter used by the renderer, the libx264
and AAC encoders, and a working ffprobe. The macOS bundler requires explicit
absolute tool paths, rejects non-portable dynamic dependencies, copies both
binaries into the application resources, and signs the nested code before the
application. Linux packages both tools in the AppImage and Windows places both
adjacent to the executable. The shared release verifier is covered by a real
process test for both acceptance and missing-capability refusal.

### G29 — Close the Menivor web timeline export lifecycle — DONE

The advanced web timeline waits until metadata belongs to the currently loaded
source before mounting, and a source change resets duration, playhead, EDL,
pending command, and export state. Every normalized EDL has a stable signature:
an exact retry after an unknown/failure outcome reuses its idempotency key,
editing the command rotates the key, and an in-flight guard prevents concurrent
double submission. A late response for a previous source is ignored.

The accepted export id is no longer discarded. It enters the existing job poll
path and the editor surfaces live progress, render failure, job detail, and the
completed signed download URL. Browser-side validation now mirrors server caps
for 200 segments, 50 overlays, finite/non-negative timing, source duration,
speed, and text length instead of relying on HTML input attributes alone.

### G30 — Make Menivor web reel creation single-flight — DONE

The canonical nine-step wizard now snapshots and fingerprints the exact job
command before asynchronous authentication. A same-frame click burst can start
only one request; an exact failed retry reuses its idempotency key, while edits
rotate the key. The accepted response clears the session B-roll handoff only
after server acceptance, and job/batch identifiers are encoded before becoming
navigation path segments.

The request builder is directly exercised across BYOV versus TTS, AI music
versus catalog music, workspace charging, signed generation profiles, variants,
and effects. Component tests drive the actual nine-step controller through
required upload/script/avatar/voice state, unknown failure/retry, command edit,
single-flight acceptance, cleanup, and final navigation.

### G31 — Make Menivor native publishing exactly retryable — DONE

Immediate native publish and future scheduling now normalize one typed command
before authentication. Both mutations are single-flight, retain one idempotency
key across an exact failed/unknown retry, and rotate the key after any form edit.
The browser refuses invalid or past schedule times before network I/O and mirrors
the backend's 200-character title, 2,200-character caption, and 30-hashtag caps.

Scheduling deliberately carries only the rendered job, platform, and UTC time;
the mounted form now says plainly that immediate title/caption/hashtags are not
persisted by the scheduler instead of silently implying otherwise. Platform
refresh preserves a still-valid selection. Focused tests cover normalized and
deduplicated metadata, local refusal, retry-key behavior, single-flight deferred
acceptance, and the exact scheduled API command.

### G32 — Idempotently start and observe wallet-metered renders — DONE

`POST /render/jobs` now binds an optional caller-scoped `Idempotency-Key` to the
normalized render command using the shared durable request ledger. Exact replay
returns the persisted envelope without a second wallet hold or provider submit;
changed payload reuse is a 409. Unkeyed compatibility remains for older clients.
The web surface always sends a stable key, blocks concurrent start/actions, and
retains the key after ambiguous failure. It now polls owner-scoped GET status
every three seconds until terminal, aborts stale timers, and contains refresh
errors instead of leaking rejected promises.

### G33 — Meter direct Rig streams without creating a second dynamic pricer — DONE

`PaidCompletionModel` remains a deliberately fixed-price direct-Mint
compatibility path, separate from Relay-owned dynamic token pricing. It now
authorizes a hold before provider stream creation, preserves incremental text,
tool, reasoning, and terminal events, captures the configured price only after
Rig yields a terminal provider response, and releases on creation failure,
provider error, missing terminal state, cancellation, or early consumer drop.

The drop reconciler carries one monotonic terminal command. Before success that
command is release; after a terminal response it becomes capture and can never
flip back. A lost capture response therefore fails the consumer closed and
retries capture idempotently instead of releasing already-served inference for
free. The same rule now protects buffered direct completions. Relay-backed
models remain on `RelayModel::connect_paid`, where Relay owns the dynamic hold,
usage rating, settlement outbox, and proof.

## Verification performed for this graph

- Menivor: the prior 50 focused API/Prism/identity tests plus 18 attestation/
  transfer tests, 200 database tests, and 33 focused transfer/attestation/
  reconciliation/workspace tests after the leased worker and account graph were added; shared contract
  build; web typecheck and all 117 web tests; 16 focused mobile wallet
  contract/pending/outcome tests; 29 `rf-gateway` unit + 13 parity tests; 10
  `rf-encode`, 53 desktop (including selected-source race/isolation and two real
  FFmpeg EDLs), and 38 durable-runtime tests.
  Repository-wide Expo compilation remains intentionally deferred. Desktop `cargo check` passes with
  `gpui/runtime_shaders`; the plain shader path remains locally blocked by the
  missing Xcode Metal Toolchain, not an application compile failure.
- G19 account-address destinations: 35 focused backend transfer/attestation/
  reconciliation tests pass, including a personal-to-workspace address journey,
  exact replay, stable balances, typed activity, unknown-address refusal, and
  legacy challenge compatibility. Alembic head is `f2c8d5e03a71`; focused Ruff
  and the shared TypeScript contract build pass. Web typecheck plus 22 wallet
  tests, 18 Expo wallet tests, 31 `rf-gateway` unit plus 13 parity tests, and 26
  GPUI application tests pass. The GPUI app checks with runtime shaders; the
  deferred native Expo installation boundary is unchanged.
- G20 external clipping: all 40 clipping policy/service/router tests pass,
  including HTTPS syntax, unconditional private-network refusal, public and
  internal redirect handling, content-type/size/empty cleanup, atomic streaming,
  safe source references, durable per-clip/stitched uploads, and promotion from
  the durable finished artifact. Focused Ruff and the generated OpenAPI contract
  checks pass.
- G21 provider-bound wallet funding: 27 focused checkout/webhook tests, 78
  wallet journey tests, and the combined 278 database + wallet tests pass,
  covering exact idempotency binding, durable checkout
  replay, fractional-cent refusal, Stripe checkout construction, provider/ref/
  amount/currency refusal, local grant, Mint grant, and provider-namespaced Mint
  deposit replay. Ruff, generated OpenAPI export/check/breaking analysis, shared
  contract build, web typecheck + 22 wallet tests, and 18 Expo wallet tests pass.
- G22 packaged backgrounds: all 11 catalog/materialization/media tests and the
  29-test background + story-template slice pass. Each checked-in MP4 is probed
  as H.264, 720×1280, 30 fps, four seconds; a two-run SHA-256 comparison proves
  byte-identical regeneration. Ruff and diff checks pass.
- G23 native identity assets: all 4 config/media invariants pass, including
  exact dimensions, opaque full icon, real adaptive alpha, locked background,
  plist linkage, ICNS/ICO readability, and Linux recipe naming. Two-run SHA-256
  output is byte-identical; shell syntax, plist validation, Ruff, 18 Expo wallet
  tests, and diff checks pass. Native Expo compilation remains deferred.
- G24–G28 desktop timeline export, source analysis, local execution, signed
  updates: all 18
  `rf-project`, 10 `rf-encode`, 13 `rf-captions`, 38 durable `rf-runtime`, and
  all 53 GPUI
  application tests pass, including exact decimal parsing, backend-bound
  collection caps, ordered EDL serialization, accepted-export reducer state,
  stale-analysis refusal, duration-bound trims, source digest binding,
  restart-persisted timeline fingerprinting, verified atomic outputs, a real
  FFmpeg waveform extraction, and real local FFmpeg artifacts from both silent
  and audio-bearing sources.
  The desktop binary checks with `gpui/runtime_shaders`; the plain local shader
  path remains dependent on the Xcode Metal Toolchain. The installed FFmpeg has
  no libass filter, so real caption burn-in is packaging-blocked here rather than
  silently downgraded.
  The 12 updater unit tests plus the OpenSSL signer integration test cover
  signature tamper, canonical parity, channel/key/target mismatch, downgrade,
  replay, size/digest corruption, atomic staging, restart rebinding, and
  argument-safe installer selection.
  The media-tool release verifier accepts a complete synthetic toolchain and
  rejects a pair missing libass; the installed development FFmpeg is likewise
  rejected for its missing `ass` filter as intended.
- G29 web timeline lifecycle: web typecheck and all 23 files / 107 tests pass.
  The 7 focused editor tests cover server-bound validation, speed-adjusted output,
  exact retry keys, command-change rotation, concurrent-submit refusal, source
  reset, stale-response isolation, and completed-download/job surfacing.
- G30 web reel creation: web typecheck and all 24 files / 111 tests pass. The 4
  focused wizard tests cover mutually exclusive request fields, wallet/profile
  funding, variants/effects, encoded routes, single-flight acceptance, retained
  exact-retry keys, edited-command key rotation, and post-acceptance cleanup.
- G31 native publishing: web typecheck and all 25 files / 115 tests pass. The 4
  focused tests cover metadata normalization/deduplication, server-aligned caps,
  past-time refusal without I/O, exact-retry key reuse, edit rotation, truthful
  schedule copy, single-flight scheduling, and UTC command submission.
- G32 wallet-metered render: all 3 render-router integration tests, web typecheck,
  and all 26 files / 117 web tests pass. Replay/changed-key behavior, one provider
  start, stable client retry keys, and automatic terminal polling are covered.
- G33 direct paid Rig streaming: all 13 `agent-core`, 21 `aitp-client`, and 16
  `relay-provider` tests pass. Coverage includes authorize-before-stream,
  terminal-only capture, creation/error/missing-terminal release, early-drop
  reconciliation, and ambiguous buffered/stream capture retry with no opposite
  release. Full workspace Clippy with all targets/features passes with warnings
  denied.
- G8 media settlement: 64 focused API/identity/Mint/attestation tests and the
  broader 72-test gateway/client-contract slice pass; focused Ruff, Alembic head
  `d7a4b91ce820`, shared TypeScript contract build, web typecheck + 6 proof tests,
  and desktop `rf-gateway` 30 unit + 13 parity tests pass. Changed mobile modules
transpile without native dependencies; the deferred Expo build boundary is
unchanged.
- Mint: 4 attestation unit, 8 binary, and 13 API tests pass. Transfer responses
  expose the exact signed sender receipt plus a byte-stable privacy-safe Ed25519
  attestation; public key discovery and identical replay are covered. The
  Postgres replay path now orders by global journal sequence, resolves transfer
  legs by requested account roles, and uses the persisted first sequence; the
  unequal-account-history regression requires a live `DATABASE_URL` to execute.
- G15 public-address transfer: 3 focused Mint HTTP tests pass (including
  malformed/unregistered destinations, cryptographic evidence, exact replay and
  changed-body conflict), all 3 `mint-sdk` tests pass, and all 20 Menivor
  `MintClient` tests pass. `mint-devserver` builds successfully. The new real-TCP
  Menivor test is collected and self-skips here only because this managed
  sandbox rejects `127.0.0.1:0` with `EPERM`; it runs rather than mocks when
  loopback is available.
- Mint hold settlement adds 8 passing server-library and 13 passing HTTP API
  tests covering capture/release command binding, positive released amount,
  privacy-safe commitments, tamper/input-drift refusal and byte-identical replay.
- Relay/agent-runtime: 33 `relay-hedge` tests; 87 `relay-billing` tests; 37 focused gateway billing
  tests; 6 durable-outbox ownership, batch-poller, capture-vs-release and fencing tests; and
  all 20 `aitp-client` library/integration tests
  pass. Coverage includes exact resolved-payer replay, full authorized hold
  amount, payee-address mapping, strict JSON/SSE usage, terminal-frame durability,
  missing-usage release, owner-scoped evidence, and duplicate execution refusal.
  The TypeScript bundle validator reports that local `tsc` is absent and skips
  that optional validation. TensorZero's `ts-executor-pool` build now accepts
  explicit offline `TENSORZERO_TYPESCRIPT_JS_PATH` and
  `TENSORZERO_SES_JS_PATH` inputs, verifies the same pinned BLAKE3 hashes, reuses
  matching sibling Cargo output artifacts across build/test/Clippy profiles, and
  otherwise retains its normal unpkg download. With the verified local cache,
  a full production `cargo check -p gateway` passes without network access.
- G16 high-level dynamic payment: all 20 `aitp-client` library/integration tests,
  all 13 `relay-provider` tests, all 87 `relay-billing` tests and all 37 focused
  gateway billing tests pass. Coverage includes policy rejection, authoritative
  retry-header replacement, funded-one-call, exact-body one-retry,
  second-402 termination, TensorZero selector hold/settlement pricing and the
  conservative unresolved-function boundary. The 6 `agent-core` tests and live
  agent example check also pass.
- G17/G18 paid streaming and evidence: the 16 `relay-provider` tests include separate-chunk text
  delivery, terminal usage, owner-vs-payer evidence, exact-body one-retry,
  second-402 termination, application-error stickiness, missing-usage refusal,
  missing-receipt refusal, mid-body disconnect, early-drop reconciliation,
  multipart exact replay, and tampered Relay projection refusal. All 21
  `aitp-client` library/integration tests and all 6 `agent-core` tests remain green.
- Identity Lock: external attestation/idempotency/owner tests.
- Turnstile: complete workspace tests and Postgres-feature compile.
- Abacus: complete workspace tests, including restart context preservation,
  exclusive recovery leases, lease-loss refusal, and transactional released
  receipt projection.
- `rit`: 56 unit + 33 real-Git interoperability tests (89 total), including
  nested ignore rules, byte-identical recursive trees, commits readable by real
  Git, collision-safe checkout, index restore, symlink confinement, pkt-line and
  capability hostility, quarantine hash/connectivity/type failures,
  non-fast-forward refusal, overlay fast-forward, full/thin pack import,
  checksum/truncation/missing-base rejection, a complete stock-Git process push,
  server-enforced force-push refusal, loose-ref deletion, packed-ref deletion
  refusal, clone/incremental-fetch through the installed-Git upload-pack bridge,
  and promoted graphs accepted by stock `git fsck --strict`.
