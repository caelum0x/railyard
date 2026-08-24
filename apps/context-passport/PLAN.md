# Context Passport Product and Architecture Plan

Last revised: 23 August 2026
Tracking issue: `videogen-n3p`

## Decision record

Context Passport is a production ReelForge platform product for user-owned AI
context portability. It is unrelated to GetVisa AI and does not contain travel,
immigration, passport, or appointment functionality.

The product owns its identity, encryption, consent, grants, receipts, developer
applications, exports, and deletion records. It may use Relay for model calls,
but it does not silently read ReelForge user content or share application tables
with other ReelForge products.

The mistaken top-level `vizo-ai` scaffold is not the implementation. Generic
configuration may be reviewed later, but product code is not copied from it
without a tracked design and security review.

## Initial customer and promise

The first users are AI power users who repeatedly explain durable preferences,
goals, accessibility needs, communication style, and working conventions to
different AI products. The first developer is one external AI application that
needs a read-only context pass.

The promise is: store structured context once, see its source and history, then
grant an approved application access to selected fields for a limited purpose
and duration. The user can inspect every read and revoke access immediately.

Automatic memory extraction, cross-app writes, behavioral prediction, hidden
profile enrichment, and data brokerage are not part of the first release.

## User workflow

1. Create an account with passkey or verified email and configure recovery.
2. Create passport fields from explicit typed input. Each field records its
   category, value, source, creation time, update time, sensitivity, and version.
3. Review a developer application's verified name, origin, privacy policy,
   requested fields, purpose, duration, and deletion commitment.
4. Approve or deny each requested field. The authorization result is an
   expiring, revocable grant, not blanket account access.
5. The application exchanges an authorization code with PKCE for a short-lived
   access token and rotating refresh credential.
6. Each read returns only the granted fields and creates a user-visible receipt.
7. The user can revoke the grant, view denied or suspicious requests, export
   their passport, delete individual fields, or delete the account.

## First release surfaces

- User dashboard for fields, provenance, versions, active grants, read receipts,
  security alerts, export, and deletion.
- Developer console for application registration, verified redirect origins,
  secret rotation, requested-scope definitions, webhook configuration, sandbox
  credentials, usage, and incident contacts.
- Consent screen with field-level selection, purpose, expiry, app identity, and
  plain-language risk information.
- OAuth-style authorization code and PKCE endpoints, token exchange, refresh
  rotation, introspection, and revocation.
- Read-only context API with typed schemas, strict audience/scope checks,
  idempotency where applicable, and per-read receipts.
- TypeScript SDK and framework-neutral HTTP examples. Additional SDKs wait for a
  real integration need.
- Protected operator tools for app suspension, credential revocation, security
  investigation, receipt lookup, deletion verification, and incident response.

## Data model

Neon PostgreSQL stores:

- users, authenticators, recovery methods, sessions, and security events;
- passport field definitions, encrypted values, sensitivity, provenance, and
  immutable versions;
- developer organizations, applications, verified origins, redirect URIs,
  public keys, contacts, and status;
- requested scope templates, consent decisions, grants, access-token hashes,
  refresh-token families, rotations, and revocations;
- access receipts, denial events, alerts, webhook outbox entries, exports,
  deletion jobs, payment state, and immutable audit events.

Raw field values are never copied into analytics, logs, search indexes, webhook
payloads, or payment metadata. Audit records use stable identifiers and field
names or categories, not decrypted values.

## Encryption and credentials

Passport values use application-level envelope encryption. Each passport has a
random data-encryption key. Ciphertext and wrapped keys are stored separately in
PostgreSQL, with key versions and authenticated metadata binding user, passport,
field, and version identifiers.

The first deployment keeps versioned wrapping keys in the Worker secret store.
Rotation re-wraps data keys rather than decrypting and rewriting every field.
Production logs never expose keys, tokens, authorization codes, plaintext
fields, or complete request bodies.

Access and refresh credentials are stored as hashes. Authorization codes are
single-use and short-lived. Refresh-token rotation detects reuse and revokes the
entire credential family. Developer secrets are shown once and can be replaced
without downtime. Operator access requires phishing-resistant MFA.

## Authorization contract

Every context read must prove:

1. token signature and lifetime;
2. issuer, audience, application, tenant, and user binding;
3. active grant and refresh family;
4. exact field scope and allowed operation;
5. purpose and grant expiry;
6. application and user status;
7. rate and anomaly limits.

The system returns no field when any check fails. Revocation invalidates new
reads immediately. Cached decrypted context is prohibited outside the request
lifetime. Integrators contractually commit to their own retention and deletion;
the platform cannot claim to erase copies already lawfully delivered to another
controller, so consent screens state this limit clearly.

## Cloud architecture and cost boundary

- Next.js browser surfaces deploy through the approved Cloudflare adapter only
  after a production proof; static assets remain static.
- A small Cloudflare Worker owns authorization, API routing, validation, rate
  limits, and service bindings. It receives explicit CPU and request-body limits.
- Neon PostgreSQL is the transactional source of truth. Connection use is
  bounded and pooled through an approved serverless-safe path.
- Private R2 stores encrypted export archives only when direct streamed exports
  cannot satisfy reliability. Passport fields do not become R2 documents.
- A PostgreSQL outbox plus scheduled Worker handles receipts, webhooks, alerts,
  and deletion recovery. Queues require measured need; Workflows and Workers AI
  are disabled for the first release.
- Email uses an approved transactional provider with verified SPF, DKIM, DMARC,
  bounce handling, and rate limits.

Context Passport shares the account-wide $12 Cloudflare ceiling. Its operating
allocation must fit inside the existing Workers Paid plan. It receives no
dedicated paid Cloudflare add-on. Application kill switches stop optional
webhooks, exports, and developer onboarding before cost threatens payment,
security, revocation, or deletion paths.

## Payments

The user-owned passport and one active developer integration remain free in the
first trust-building release. Later developer plans may charge for active users,
applications, and API volume through Creem as the web merchant of record.

Payment state is webhook-driven, signature-verified, idempotent, and separate
from authorization truth. Expired billing can block new commercial reads after
a grace period but cannot block user revocation, export, or deletion. RevenueCat
is considered only if a native mobile subscription ships; it is not added to
the web product merely for speed.

## Threat model

The release must address:

- malicious applications requesting excessive scopes or misleading users;
- redirect URI substitution, authorization-code interception, CSRF, confused
  deputy behavior, token theft, refresh reuse, and audience confusion;
- tenant or user IDOR, stale authorization caches, privilege escalation, and
  support impersonation;
- inference of hidden fields from metadata, receipts, errors, or field presence;
- compromised webhooks, replay, SSRF, unsafe URLs, and log injection;
- plaintext leakage through analytics, model calls, backups, exports, browser
  storage, error reporting, or customer support;
- incomplete deletion, failed key rotation, lost recovery credentials, and
  unauthorized restore from backup.

Security tests include cross-tenant property tests, token protocol tests,
adversarial consent copy review, dependency and secret scanning, external
penetration testing, backup restore, key rotation, and verified deletion.

## Production observability

Measure authorization success and denial by reason, token refresh/reuse,
revocation latency, receipt-write latency, webhook delivery, encryption errors,
database saturation, Worker CPU, request volume, export/deletion age, and
projected cost. Metrics never contain passport values.

Alerts cover anomalous application reads, credential reuse, cross-tenant denial,
failed audit writes, revocation latency, stuck deletion, key errors, payment
webhook failures, and budget thresholds. An audit-write failure blocks the
context read instead of serving unreceipted data.

## Release sequence

### Stage 0: contracts and threat model

Freeze field taxonomy, data-controller roles, token/grant protocol, receipt
schema, deletion semantics, privacy terms, threat model, and cost envelope.

### Stage 1: local single-user passport

Ship authentication, encrypted typed fields, provenance, versions, recovery,
export, and deletion without third-party access.

### Stage 2: sandbox developer integration

Ship application registration, verified origins, authorization code with PKCE,
field-level consent, scoped read API, receipts, refresh rotation, and revocation
against generated non-sensitive data.

### Stage 3: external integration

One external developer integrates the sandbox and passes origin, scope,
revocation, token-reuse, webhook, export, and deletion tests. Complete privacy
and security review before real user context.

### Stage 4: limited production

One real user grants exactly two non-sensitive fields to the reviewed app. Prove
a successful receipted read, denied out-of-scope read, immediate revocation,
failed token reuse, export, deletion, backup restore, and key rotation.

### Stage 5: commercial developer plan

Add Creem only after the trust and reliability gates pass. Verify checkout,
signed duplicate/out-of-order webhooks, entitlement reconciliation, grace
period, refund, cancellation, and continued revocation/export/deletion access.

## Non-goals for the first release

- automatic extraction from conversations, files, video, or browsing history;
- write access from third-party applications;
- a marketplace of unreviewed applications;
- advertising, data resale, training-data export, or audience enrichment;
- storing passports, identity documents, or immigration information;
- using AI to decide consent, scope, access, security response, or deletion;
- claiming that downstream applications deleted copies without evidence.

## Pre-code gate

Before implementation:

1. approve the field taxonomy and prohibit high-risk identity, health, finance,
   biometric, child, and precise-location fields from the first release;
2. approve controller/processor roles, privacy terms, retention, deletion, and
   downstream-app obligations with qualified review;
3. freeze the authorization, token rotation, receipt, encryption, and key
   rotation specifications;
4. complete threat modeling and produce generated evaluation fixtures;
5. confirm the first external developer and its exact two-field integration;
6. approve the Cloudflare/Neon cost proof and failure kill switches;
7. define support, incident response, recovery, and app-suspension procedures.
