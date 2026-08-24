# ranguard — Enterprise Architecture

> **Scope of this document.** The *target* enterprise shape for `ranguard`: an
> integrity / attestation **platform** — signed manifests, process attestation, an
> anti-replay heartbeat protocol, a policy engine, telemetry/reporting, an admin
> console, and an SDK games/apps embed. This is a **planning artifact** — it does
> not change app code. Today `ranguard` is a single crate + binary (`src/{sha256,
> hmac,manifest,process,heartbeat,report}.rs`, 16 tests) that demonstrates the
> *honest, userspace* subset of anti-cheat with hand-rolled crypto and zero
> dependencies. The tree below promotes those proven primitives into a
> productized, multi-service platform **while preserving the safety posture**
> (`#![forbid(unsafe_code)]`, consent-based, no kernel driver, no foreign-process
> snooping).

---

## 1. Design invariants (carried from the current README)

- **Safe userspace only.** No kernel driver, no ring-0, no foreign-process memory
  scanning, no anti-debugging of arbitrary PIDs. Attestation only ever consults a
  `Child` handle the tool owns and a sandbox it created. `#![forbid(unsafe_code)]`
  stays crate-root across the workspace.
- **Real crypto at the enterprise tier.** The from-scratch SHA-256/HMAC are correct
  (RFC 4231 / FIPS 180-4 vectors) and stay as the dependency-free *reference*, but
  the platform tier adds **audited crypto** (`sha2`, `hmac`, and Ed25519 for real
  signatures with key management) — the README itself flags HMAC-shared-secret and
  SplitMix64 nonces as demo-only limitations to fix.
- **Deterministic, canonical signing.** The manifest's canonical body (fixed field
  order, path-sorted entries, streamed 8 KiB hashing) is the reproducibility crux
  and is preserved verbatim — verification recomputes identical bytes.
- **Anti-replay by construction.** Strictly-increasing counters + rolling-HMAC
  chains + nonce binding; constant-time comparison on every MAC/hash check.
- **Consent + transparency.** Every action is auditable and scoped to what the
  operator explicitly enrolled. No stealth, persistence, or evasion.

---

## 2. Target workspace tree

`ranguard` becomes its own Cargo workspace (isolated from the desktop build). The
`rg-core-*` crates are the productized primitives; services + SDK sit above them.

```text
apps/ranguard/
  Cargo.toml                      # [workspace] members = crates/* + services/*
  rust-toolchain.toml
  docs/{THREAT-MODEL,SAFETY-POSTURE,PROTOCOL}.md
  crates/
    rg-crypto/                    # crypto layer: the audited path (sha2/hmac/
                                  #   ed25519) PLUS the vendored reference impls
                                  #   (today's sha256.rs/hmac.rs) behind a feature.
                                  #   constant_time_eq, CSPRNG nonces (replaces
                                  #   SplitMix64), key derivation.
    rg-manifest/                  # signed integrity manifest (today's manifest.rs):
                                  #   canonical body, streamed hashing, build/scan/
                                  #   diff, signature verify. Ed25519-signable.
    rg-attest/                    # process + environment attestation (today's
                                  #   process.rs): owned-child liveness, on-disk
                                  #   binary re-hash, attestation records/evidence.
    rg-heartbeat/                 # anti-replay challenge/response protocol (today's
                                  #   heartbeat.rs): counters, rolling MAC chain,
                                  #   nonce binding, session state machine.
    rg-protocol/                  # the WIRE protocol the demo lacked: framed
                                  #   enroll/attest/heartbeat/revoke messages,
                                  #   versioning, codecs. Transport-agnostic.
    rg-policy/                    # policy engine: declarative rules over manifest
                                  #   findings + attestation outcomes + heartbeat
                                  #   liveness → verdict (allow/flag/revoke) with
                                  #   reason codes. The decision kernel.
    rg-keys/                      # key management: issuance, rotation, revocation,
                                  #   per-tenant signing keys, verification-key
                                  #   registry (the piece the demo hardcoded).
    rg-telemetry/                 # structured integrity events + evidence bundles;
                                  #   redaction; the reporting data model.
    rg-audit/                     # append-only audit log of every verdict, enroll,
                                  #   revoke — the accountability record.
    rg-store/                     # repository-pattern persistence (sessions,
                                  #   manifests, attestations, verdicts, keys).
    rg-report/                    # human + machine reporting (today's report.rs):
                                  #   change kinds, tamper logs, rollups.
    rg-sdk/                       # embeddable client SDK for games/apps: enroll,
                                  #   sign a manifest, run attestation, drive the
                                  #   heartbeat, submit evidence. #![forbid(unsafe)].
  services/
    rg-server/                    # the attestation service: verification-key
                                  #   registry, heartbeat session mgmt, policy
                                  #   evaluation, verdict issuance, telemetry sink.
    rg-console/                   # admin console (web): fleet view, enrolled apps,
                                  #   live sessions, verdicts, policy editor, audit.
    rg-agent/                     # optional host-side agent that runs enrolled-app
                                  #   attestation locally and reports to rg-server.
  bin/
    ranguard-demo/                # today's scripted end-to-end demo (main.rs),
                                  #   preserved as the honest teaching artifact.
  tests/
    vectors/                      # RFC 4231 / FIPS 180-4 + protocol conformance
    e2e/                          # enroll→attest→heartbeat→verdict→revoke flows;
                                  #   replay + forgery + tamper rejection.
```

### Dependency flow

```text
game/app  ──embeds──▶ rg-sdk ──rg-protocol──▶ rg-server
   │                    │                         │
   │  rg-manifest / rg-attest / rg-heartbeat      ├─▶ rg-policy ──▶ verdict
   │  (all over rg-crypto + rg-keys)              ├─▶ rg-keys (verify registry)
   │                                              ├─▶ rg-telemetry / rg-audit
   └── rg-report (local)                          └─▶ rg-store
                                            rg-console ──reads──▶ store/audit/telemetry
```

---

## 3. Phased build order

| Phase | Delivers | Exit gate |
|-------|----------|-----------|
| **R0** Harden primitives | `rg-crypto` (audited path + CSPRNG + Ed25519), split `rg-manifest`/`rg-attest`/`rg-heartbeat`/`rg-report` out of the mono-crate | Existing 16 tests pass unchanged; Ed25519 signing + real nonces added; reference impls still vector-verified |
| **R1** Protocol + keys | `rg-protocol` wire codecs, `rg-keys` (issue/rotate/revoke + verification registry) | Enroll→heartbeat→revoke over a real transport; replay/forgery rejected on the wire |
| **R2** Policy + persistence | `rg-policy`, `rg-store`, `rg-audit`, `rg-telemetry` | Findings → deterministic verdict with reason codes; every verdict audited |
| **R3** Service + SDK | `rg-server`, `rg-sdk` | A sample app enrolls via SDK; server issues live verdicts; anti-replay holds across sessions |
| **R4** Console + agent | `rg-console`, `rg-agent`, fleet reporting | Operator sees fleet, edits policy, revokes a compromised session |
| **R5** Production | independent security review, key-management review, incident response | External review of the (now non-demo) crypto + tenancy passes |

---

## 4. Graph engineering layer

Integrity is a knowledge graph, and the enforcement loop is a task graph. Modeling
both (per `~/.claude/skills/graph-engineering/references/`) makes the attestation
chain an explicit, queryable provenance graph rather than scattered checks.

### 4.1 Integrity knowledge graph (ontology)

**Competency questions:** Which sessions of app `A` are currently live and
attested? What changed between manifest `M1` and `M2` for enrolled app `A`? Which
heartbeats in session `S` were rejected, and why (stale/nonce/MAC)? What is the
full provenance chain from an enrolled binary to its latest verdict? Which key
signed manifest `M`, and is that key still valid?

**Entities:** `App` (enrolled game/app), `Session`, `Process` (attested owned
child), `Manifest`, `ManifestEntry` (attribute-bearing: path,size,sha256),
`Attestation`, `Heartbeat`, `Challenge`, `SigningKey`, `Verdict`, `AuditEvent`.

**Relations (verb, domain → range, with time + provenance on every edge):**

| Relation | Domain → Range |
|----------|----------------|
| `ENROLLS` | App → Manifest (version attr) |
| `SIGNED_BY` | Manifest → SigningKey (Ed25519; the tamper-evidence root) |
| `CONTAINS` | Manifest → ManifestEntry |
| `OPENS` | App → Session (start time) |
| `ATTESTS` | Attestation → Process (outcome: Ok/Exited/BinaryTampered) |
| `IN_SESSION` | {Attestation, Heartbeat} → Session |
| `ANSWERS` | Heartbeat → Challenge (counter + nonce attrs) |
| `CHAINS_FROM` | Heartbeat → Heartbeat (rolling-MAC prev link) |
| `YIELDS` | {Attestation, Heartbeat, Scan} → Verdict (reason code) |
| `RECORDS` | AuditEvent → {any entity} |

**The attestation chain *is* a provenance graph.** `SIGNED_BY` roots every manifest
in a key; `CHAINS_FROM` makes the heartbeat sequence a hash-linked chain (each
accepted tag folded into the next — a per-session Merkle-ish spine); `YIELDS` +
`RECORDS` bind every verdict to its evidence and to an append-only audit node.
Time + provenance ride every edge by construction (counters, nonces, timestamps,
key ids) — the "decide provenance at modeling time" rule is satisfied because the
crypto *is* the provenance. `rg-policy` is a query over this graph ("is there an
unbroken attested+heartbeat path for session `S` under a valid key?"); a broken
edge (tamper, replay, forgery, exit) is exactly a missing/invalid graph edge.

### 4.2 The attest→heartbeat→verify→revoke task graph

The enforcement loop is a bounded, verifier-separated diamond with a gate on the
irreversible revoke edge:

```text
enroll (sign manifest, register key)
        │
        ▼
   ┌─ manifest scan (tamper diff) ─┐
   ├─ process attest (owned child) ─┼─→ VERIFY (rg-policy: separate context,
   └─ heartbeat round (anti-replay)─┘     evaluates all evidence → reason code)
        ▲                                        │
        │  loop: next challenge (max rounds)     ▼
        └──────────  Accepted  ◀──────── verdict = allow ─┐
                                          verdict = flag ──┤ (telemetry/audit)
                                          verdict = revoke ─┘
                                                  │
                            [GATE: revoke is irreversible → policy threshold
                             (or operator confirm in rg-console) required]
                                                  ▼
                                    revoke session + audit (owned by rg-server)
```

- **Parallel workers** = the three independent evidence sources (manifest scan,
  process attestation, heartbeat) never read each other's output → genuine fan-out.
- **Separate verify node** = `rg-policy` evaluates evidence in its own context and
  emits reason codes — the checks never grade themselves.
- **Human/policy gate** on the *irreversible* revoke edge (kicking a player /
  disabling an app), not on every heartbeat — placement per the gate rule.
- **One owner of the verdict** = `rg-server` issues and audits it; agents/SDKs never
  self-adjudicate (stop-rule: one owner of the merge).
- **Guardrails:** the heartbeat loop has a max-round cap; counters strictly
  increase; the audit append is transactional with the verdict.

---

## 5. Enterprise scale estimate

**Today `ranguard` is ~8 source files** — a faithful *demo* of three primitives. An
enterprise integrity/attestation **platform** (key management, wire protocol,
policy engine, multi-tenant server, admin console, embeddable SDK) is realistically
**hundreds of files, ~40K–70K LOC** — smaller than a forge, larger by 10× than the
demo. The primitives are real; the platform around them is what's missing.

| Crate / service | ~Files | ~LOC |
|-----------------|-------:|-----:|
| rg-crypto | 12 | 4,500 | *(today's sha256.rs/hmac.rs)* + audited + ed25519 |
| rg-manifest | 8 | 3,000 | *(today's manifest.rs)* |
| rg-attest | 8 | 3,000 | *(today's process.rs)* |
| rg-heartbeat | 8 | 3,000 | *(today's heartbeat.rs)* |
| rg-protocol | 12 | 5,000 | wire codecs (new) |
| rg-policy | 12 | 5,500 | rule engine + reason codes |
| rg-keys | 10 | 4,000 | issue/rotate/revoke/registry |
| rg-telemetry | 8 | 3,000 | events + evidence bundles |
| rg-audit | 6 | 2,000 | append-only log |
| rg-store | 12 | 4,500 | persistence |
| rg-report | 6 | 2,000 | *(today's report.rs)* + rollups |
| rg-sdk | 14 | 5,000 | embeddable client |
| rg-server (service) | 25 | 10,000 | attestation service |
| rg-console (service) | 60 | 14,000 | admin web console |
| rg-agent (service) | 12 | 4,500 | host-side agent |
| ranguard-demo (bin) | 3 | 900 | *(today's main.rs, preserved)* |
| tests/{vectors,e2e} | 30 | 9,000 | conformance + flows |
| **Total** | **~246** | **~86,400** | (~55K excl. console+tests) |

**What "done" means per phase:** R0 (hardened primitives) ≈ 45 files; R1–R2
(protocol + policy + persistence) ≈ +90; R3 (server + SDK) ≈ +40; R4–R5 (console +
agent + review) ≈ +70. The **current 8-file demo is ~3% of the platform** and, by
its own README, is explicitly a teaching artifact — not a shippable anti-cheat.

---

## 6. SDK / interop & platform composability

**`rg-sdk`** is the embeddable contract: a game/app links it to enroll, sign a
manifest, run owned-child attestation, drive the anti-replay heartbeat, and submit
evidence to `rg-server` — all under `#![forbid(unsafe_code)]`. `rg-protocol` is the
open wire contract for non-Rust clients.

Composability with Railyard — **honest fit**:

- **ranguard attests agents on the rail.** This is ranguard's cleanest platform
  role: the **[agent-runtime](../agent-runtime)** workers, **Relay** gateway
  processes, and **[rithub](../rithub)** CI runners are exactly "processes we
  launched and own" — the one thing ranguard *can* legitimately attest. `rg-sdk`
  gives each a signed manifest + heartbeat, so the platform trusts only agents
  whose binary hash + manifest verify and whose liveness chain is unbroken.
- **Provenance ties to Passport & Mint.** A ranguard `Verdict` / attestation record
  (with its Ed25519-signed evidence) is a provenance artifact: it can be referenced
  by a **context-passport** version ("this generation ran on an attested agent") or
  anchored to a **Mint** hash-linked receipt ("this settled spend came from a
  verified process"). The binding lives in Passport/Mint; ranguard supplies the
  signed evidence.
- **Honest fit / boundary.** **ranguard is a systems/integrity tool, not a payer or
  ledger on the AITP-402 rail.** It moves no money and holds no balances. Its value
  to the rail is *trust in execution* (attested agents), which is orthogonal to and
  complementary with Mint's *trust in settlement*. Keep them decoupled: ranguard
  emits signed attestations; the payment rail decides whether to require them.
- **Reused primitives.** `rg-crypto`'s vector-verified SHA-256/HMAC and the
  canonical-signing discipline mirror the hash-linked-receipt and Ed25519 patterns
  already used by **Mint** and **agent-runtime**'s `aitp-client` — consistent
  cryptographic conventions across the rail without a hard dependency.

---

## 7. Key decisions

1. **Promote primitives, preserve posture** — the demo's proven SHA-256/HMAC/
   manifest/attest/heartbeat become productized crates; `#![forbid(unsafe_code)]`,
   consent, and no-kernel-driver stay non-negotiable.
2. **Real crypto at the platform tier** — audited `sha2`/`hmac` + Ed25519 + CSPRNG
   + key management replace the demo's shared-secret HMAC and SplitMix64 nonces
   (the README's own flagged limitations), with the reference impls retained.
3. **The attestation chain is a provenance graph** — modeled explicitly so
   `rg-policy` is a graph query and every verdict is evidence-linked and audited.
4. **Enforcement is a verifier-separated diamond with a gate on revoke** — parallel
   evidence, `rg-policy` verifies, `rg-server` owns the irreversible verdict.
5. **Attest the rail's own agents; don't couple to the ledger** — ranguard's honest
   platform role is execution trust for owned agents; provenance ties to Mint/
   Passport are additive and consumer-owned.
