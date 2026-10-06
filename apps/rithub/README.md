# Rithub: a self-hostable git forge in Rust

The self-hostable collaboration forge built **on top of `rit`** ([`../rit`](../rit)):
repositories, organizations, permissions, issues, pull requests, review, protected branches,
webhooks, releases, audit logs. The `GitHub` equivalent.

## Product identity

`apps/rithub` (this Rust forge, on the `rit` engine) **is the shipped Rithub
product**. `apps/rithub-forge` is a separate, optional Forgejo-based deployment
kept for teams that need Forgejo-only features; it has its own database and is
not connected to this server.

## Security model (web + API)

- **Accounts:** self-service sign-up at `/signup` (creates the user, a personal
  organization, and a first access token shown once) unless
  `RITHUB_OPEN_SIGNUP=false`; OAuth/SSO via GitHub/Google when configured.
  Site admins are listed in `RITHUB_ADMIN_USERS`.
- **Sessions:** the browser cookie holds a dedicated, revocable, 30-day session
  token (never a PAT); sign-out revokes it server-side. Session tokens are
  rejected by git transport and API bearer auth.
- **API auth:** `Authorization: Bearer <pat>` / `token <pat>`, HTTP Basic
  `user:pat`, or the web session. Authorship always comes from the
  authenticated identity, never from request bodies.
- **Authorization:** private repositories are invisible (404) without access on
  every surface (web, API, git, GraphQL, search). Writes require the effective
  repo role (org role + collaborator + team grants): Write for push/merge/
  triage/releases, Admin for settings/webhooks/deploy keys/runners/protection;
  org owners manage membership. PR approvals only count from writers.
- **CSRF:** state-changing requests with a foreign `Origin` or
  `Sec-Fetch-Site: cross-site` are refused; cookies are `SameSite=Lax`,
  `HttpOnly`, and `Secure` behind HTTPS (`RITHUB_PUBLIC_URL=https://...`).
- **Webhooks:** targets are validated against private/loopback networks at save
  and delivery time (override with `RITHUB_WEBHOOK_ALLOW_PRIVATE=1`), no
  redirects are followed.

## Billing (Rago + Mint)

`crates/rithub-server/src/billing.rs` uses the same Rago contract as the
Forgejo bridge: one org = one Rago customer/subscription (`sub_<org>`). Creating
or flipping a repo to private is refused with **402** when the org's
`private_repos.max` entitlement is reached (fail-closed 503 if Rago is down),
and private-repo, seat, and push usage events are posted to
`RAGO_BASE_URL/api/v1/events`. Rago invoices settle through Mint on Rago's side.
Without Rago, `RITHUB_FREE_PRIVATE_REPOS` applies a local cap (unset = unlimited).

## Status: Phase 7 — **fully self-contained git engine + richer collaboration**

rithub is a **self-contained** git forge: the entire git plane — clone, fetch,
and push over HTTP **and** SSH, three-way merge, and packfile generation — is
`rit`-native, with **no `git` binary needed on the server** (proven with `git`
off the server's PATH). On top of that it has the full collaboration core (issues,
PRs with merge/squash/rebase, review, **forks + cross-repo PRs**), a web UI,
releases, security/governance depth (RBAC, protected-push on both transports, CI
gates, audit, search), and notifications. Actions/runners, packages, wikis/
projects, OAuth/SSO, and a SPA remain ahead; see the phased map. Full shape:
[ARCHITECTURE.md](./ARCHITECTURE.md).

### Self-containment: native fetch + strategies + forks (Phase 7) — proven with `git` OFF the server PATH

- **Native `git-upload-pack` + packfile generation** (`rit::upload_pack`): parse
  the `want`/`have` negotiation, compute the send set by reachability, and emit a
  real `PACK` (per-object type/size varint + zlib, SHA-1 trailer). The **last
  `git upload-pack` bridge is removed** — `git clone`/`git fetch` over both HTTP
  and SSH are now fully rit-native. `git unpack-objects`/`fsck` validate the pack
  (interop-tested). Combined with the native merge/receive, the server hosts git
  with **no `git` binary at all**.
- **Squash & rebase merge strategies** (in addition to merge-commit/fast-forward):
  squash lands one single-parent commit of the PR's net changes; rebase replays
  the PR's commits onto base (linear history, no merge commit).
- **Forks + cross-repo PRs**: fork copies a repo's objects + refs; a PR from a
  fork's branch imports the fork head into the upstream repo (`refs/pull/<n>/head`)
  so diff/mergeability/merge resolve locally and land the fork's commit upstream.
- **Syntax highlighting**: not shipped — `syntect` isn't vendorable offline here;
  blobs/diffs remain HTML-escaped (flagged).

### Self-containment + SSH (Phase 6) — proven with real `git`

- **Native three-way merge** (`rit::merge`): merge-base + tree merge + `diffy`
  diff3 content merge → a clean merged tree oid or a conflict path list. The
  `git merge-tree` bridge is **removed** — a diverged PR now merges to a real
  two-parent commit with **`git` not on the server's PATH**, and a conflicting PR
  reports conflict, all natively. `git fsck` validates the byte-exact result.
- **SSH transport** (russh): public-key auth against registered `ssh_keys`, then
  `git-upload-pack`/`git-receive-pack` over the SSH channel with the **same RBAC +
  protected-branch enforcement as HTTP**. Real `git clone`/`git push` over
  `ssh://` work; a non-admin's direct push to a protected branch is rejected at
  receive-pack (`protected branch: direct push blocked, open a pull request`);
  unregistered keys are denied.
- **Notifications**: in-app records fanned out to a repo's audience (org members ∪
  collaborators, minus the actor) on PR opened/merged and review submitted, with
  a JSON API + web page; webhook `push` events added too.

### Security & governance (Phase 5) — proven with real `git` + API

### Security & governance (Phase 5) — proven with real `git` + API

- **Fine-grained RBAC**: per-repo collaborators (`read`/`write`/`admin`) + teams
  (team members + team→repo grants). Every git/API/web request resolves an
  **effective role** = max(org role, collaborator grant, team grants), gated
  before any object/ref is touched (read ≥Read, push ≥Write, admin ≥Admin).
- **Protected-branch PUSH blocking (at receive-pack)**: a direct `git push` to a
  protected branch by a non-admin is **rejected during receive-pack** with a
  clear remote message (`protected branch: direct push blocked, open a pull
  request`) and the pack is discarded; force-pushes (non-fast-forward) are
  rejected independently. Admins may push directly; everyone else goes via PRs.
- **CI status checks**: `POST .../commits/{sha}/statuses` (context/state/target_url)
  + a **required status checks** protection rule — a PR merge is blocked until
  every required context is `success`.
- **Audit log**: append-only records for push, push.rejected, pr.merged,
  protection.changed, collaborator/member changes, token.created, status.set —
  queryable via API + a web page.
- **Search**: repo/issue/PR search (`ILIKE`, wildcard-escaped) via API + a web page.

### Web UI (Phase 4) — server-rendered, real content, proven with `curl`

Hand-rolled HTML (no template engine) with strict escaping + markdown-sanitized
READMEs; every page renders live data from Postgres + `rit`:

- **Repo home**: file tree at HEAD, rendered README (markdown→HTML), branch
  selector, and the `git clone` URL.
- **Blob view**: a file's contents at a ref/path (escaped).
- **Commit history** for a branch + a **single-commit page** with the coloured
  unified diff (`rit::diff_trees` + `similar`).
- **Issues**: list (open/closed) + detail (comments/labels/assignees) + "New issue".
- **Pull requests**: list + detail (diff, mergeability, reviews) + a **merge
  button** that respects the protected-branch approval gate + "New PR".
- **Landing** (org/repo browse) + **login** (PAT → session cookie).
- **Releases**: draft a release (tag/name/notes over a commit), a releases page,
  and a **source `tar.gz`** produced on demand from the git tree.

### Collaboration (Phase 3) — proven with real `git` + API

- **Issues**: create/list/get/close/reopen, comments, labels, assignees, state —
  Postgres-backed, per-repo numbering under a row lock.
- **Pull requests**: open (head→base, same repo), commit list, a real **diff**
  (three-dot: `rit` tree-diff + `similar` unified patches), **mergeability**
  (fast-forward / merge-commit / conflict via `rit::is_ancestor` + `rit::merge_tree`),
  comments, state machine, and **merge** — fast-forward advances the base ref
  (atomic CAS); a diverged clean merge writes a real **two-parent merge commit**
  (`rit::Repository::commit_tree` on the merged tree). After merge the base ref
  advances and the PR closes as `merged`.
- **Review**: approve / request-changes / comment with inline thread comments;
  a **protected-branch** rule can require ≥N approvals before merge (counts the
  latest state per reviewer). Verified: merge blocked at 0 approvals → allowed
  after 1; conflicting PR reports `conflict` and merge returns 409.
- **Webhooks**: repo subscriptions delivered as **HMAC-SHA256-signed** POSTs
  (`X-Rithub-Signature-256`) with bounded retry + a delivery audit table, for
  `issue.opened` / `pr.opened` / `pr.merged`.

### Hosting (Phase 1–2) — proven with real `git`

- **Workspace** (own Cargo workspace, path-depends on `rit`):
  - `rithub-core` — forge domain (`User`, `Organization`, `Repository`,
    `Membership`, `Role`, `Visibility`, `Token`) + a repository-pattern `Store`
    trait with a Postgres (`sqlx`) implementation + name validation + PAT hashing.
  - `rithub-git` — the **only** crate that touches `rit`: initializes bare repos,
    seeds/accepts commits, and mounts the git transports (dumb HTTP + smart
    upload-pack/receive-pack).
  - `rithub-server` — an `axum` service: forge JSON API + git HTTP (smart & dumb)
    + Basic-auth/PAT authorization on every git request.
- **Forge API** (Postgres-backed, real handlers): create/list/get orgs; create/
  list/get repos (creating a repo runs a real `rit` **init-bare**); seed a commit;
  create users; add org members; mint personal access tokens.
- **Git hosting — smart protocol**:
  - **Fetch/clone**: `GET info/refs?service=git-upload-pack` + `POST git-upload-pack`
    (protocol v2), mounted on `rit`'s installed-git upload-pack bridge.
  - **Push**: `GET info/refs?service=git-receive-pack` + `POST git-receive-pack`,
    backed by `rit`'s **native** receive-pack (quarantine → connectivity check →
    atomic ref compare-and-swap). New-branch and fast-forward succeed; a
    non-fast-forward push is **rejected** by server policy (even `--force`).
  - **Dumb HTTP** retained as a fallback (`/HEAD`, `/objects/...`, `/objects/info/packs`).
- **Auth (no bypass)**: HTTP Basic `username:PAT`; tokens stored only as SHA-256
  hashes. Push requires an authenticated org **member**; clone/fetch is anonymous
  for **public** repos and member-only for **private** repos.

Verified end to end with stock `git`: smart clone (v2) → local commit →
authenticated push → fresh re-clone shows the pushed commit, `git fsck --full`
clean; anonymous push → `401 authentication required`; non-fast-forward `--force`
→ `[remote rejected] (non-fast-forward)`, remote tip unchanged; private repo →
anonymous clone `401`, member clone succeeds.

### `rit` additions made for hosting (minimal + faithful, no test regressions)

- `rit::server` — `dumb_info_refs`, `objects_info_packs`, `update_server_info`
  (the `git update-server-info` equivalent), and `upload_pack_advertise` /
  `upload_pack_rpc` (mount `git upload-pack --stateless-rpc [--advertise-refs]`
  for smart-HTTP fetch, forwarding `GIT_PROTOCOL`).
- `rit::Repository::commit_tree` — a `git commit-tree` (+ optional atomic ref CAS)
  primitive that works on a **bare** repo (unlike `commit_index`).
- Push reuses `rit`'s already-public `advertise_receive_pack` /
  `execute_receive_pack` / `ReceivePackPolicy` — no additions needed.

### Run it

```bash
docker run -d --name rithub-pg -e POSTGRES_PASSWORD=rithub -e POSTGRES_USER=rithub \
  -e POSTGRES_DB=rithub -p 55432:5432 postgres:17

cd apps/rithub
DATABASE_URL=postgres://rithub:rithub@localhost:55432/rithub \
RITHUB_DATA_ROOT=./rithub-data RITHUB_BIND=127.0.0.1:8099 \
  cargo run -p rithub-server            # migrations run on boot

# org, member user, token, repo:
curl -sX POST localhost:8099/api/orgs   -d '{"name":"acme"}'
curl -sX POST localhost:8099/api/users  -d '{"username":"ada","email":"ada@x.io"}'
curl -sX POST localhost:8099/api/orgs/acme/members -d '{"username":"ada","role":"owner"}'
TOKEN=$(curl -sX POST localhost:8099/api/tokens -d '{"username":"ada"}' | jq -r .token)
curl -sX POST localhost:8099/api/orgs/acme/repos -d '{"name":"widgets"}'

git clone http://localhost:8099/acme/widgets.git         # anonymous (public)
# ...commit locally...
git push "http://ada:$TOKEN@localhost:8099/acme/widgets.git" HEAD:main   # authenticated
```

## Honest phased map — what remains

| Area | Scope | State |
|------|-------|-------|
| **Hosting: clone/fetch/push** | dumb + smart HTTP, new-branch/fast-forward/non-ff, PAT auth, public/private | **done** |
| **Collaboration** | issues, pull requests (diff/mergeability/merge), reviews, protected-branch approval gate, webhooks | **done** |
| **Web UI + releases** | landing/login, repo browse, blob, commit diff, issues, PRs (diff + merge button), releases + `tar.gz` | **done** |
| **RBAC + protected push** | per-repo collaborators, teams, effective-role resolution, **protected-branch push blocking at receive-pack** (HTTP + SSH), force-push rejection | **done** |
| **CI + audit + search** | commit statuses, required-checks merge gate, append-only audit log, repo/issue/PR search | **done** |
| **Native merge + SSH + notifications** | from-scratch three-way merge in `rit`, SSH transport (pubkey auth, RBAC, protected-push), in-app notifications + webhook `push` | **done** |
| **Fully native git plane** | from-scratch `git-upload-pack` + **packfile generation** (last bridge removed) — clone/fetch over HTTP + SSH with no server `git` | **done** |
| **Merge strategies + forks** | squash + rebase merge strategies, forks (object copy), cross-repo (fork) pull requests | **done** |
| **Auth depth** | token scopes/expiry/revocation, deploy keys, OAuth/SSO, persistent SSH host key | partial (PAT + web session + SSH pubkey today) |
| **Automation** | webhook durable retry/DLQ, email notifications, jobs/outbox worker, CI runners | partial (in-app notifications + API/push webhooks + retry + audit) |
| **Markup / assets** | syntax highlighting (blobs shown escaped), richer/safer markdown, binary release assets, code (grep-style) search | not yet |
| **CI runner + LFS** | `rithub-runner` executes `.rithub/workflows/*.yml` jobs and reports check runs; Git LFS batch API + basic transfer (sha256-verified, RBAC-scoped); live-Postgres test harness (`RITHUB_TEST_DATABASE_URL`) | **done** |
| **Packages + wiki** | per-repo package registry: npm, generic files and OCI images (`/v2/`), content-addressed, repo permissions; git-backed markdown wiki (`{repo}.wiki.git`) with history, optimistic locking and repo permissions | **done** |
| **Big-ticket products** | pages, projects/discussions, gists, SPA | not yet |

Known limitations (not faked): the git server is now **fully `rit`-native** — the
`rit upload-pack`/`receive-pack` CLIs and the merge engine use **no `git` binary**
(proven with `git` off the server's PATH for clone, fetch, push, and merge). The
native fetch speaks protocol **v0** and emits **undeltified** (full) packs — valid
and `fsck`-clean, but larger on the wire than git's delta-compressed packs; the
SSH host key is generated fresh per boot (not persisted); JSON-API authorship
comes from a body field (the web UI attributes to the session user); token/member
events are recorded org-scoped (repo_id NULL); cross-repo PR heads are pinned at
open time (not auto-updated on new fork pushes); blobs are shown escaped without
syntax highlighting (`syntect` not vendored offline); the git plane is plain
HTTP/plain SSH (front with TLS / harden host keys for production).

## Honest "GitHub-equivalent" completeness estimate

**Roughly 40–45%** of a GitHub-equivalent forge, weighted by user-facing surface
and depth. What's solid — and now **fully self-contained**: the entire git plane
(clone/fetch/push over smart+dumb HTTP **and SSH**, a from-scratch three-way merge
**and native packfile generation**, with no `git` binary on the server), the
collaboration core (issues/PRs/review/**merge + squash + rebase**), **forks +
cross-repo PRs**, a working web UI, releases, fine-grained RBAC with **receive-pack
push protection on both transports**, CI status gates, audit, search, and in-app
notifications. What a full GitHub still has that rithub does not: Actions/CI
runners, code search & syntax highlighting, email/digest notifications, packages/
LFS/pages, projects/wikis/discussions, gists, org/enterprise admin depth, GraphQL
API, OAuth/SSO, delta-compressed packs + protocol v2, and a polished SPA front end.
By raw scale this is ~20–26k LOC vs Gitea's ~500k+ / GitLab's millions — but the
**architecturally hardest, security-critical spine (a from-scratch git object/ref/
merge engine + native pack read AND write, two authenticated transports, and
receive-pack protection) is complete**, which is disproportionately the valuable
part.
