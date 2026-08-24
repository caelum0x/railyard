# Vend production operations

This runbook covers the currently supported `tenant_reserve` product boundary.
It does not claim that a local or sandbox run proves a production deployment.

## Release and migration

1. Build an immutable image from the repository root and record its digest.
2. Take and restore-test a Postgres custom-format backup before schema changes.
3. Run one `vend-server` instance against a staging clone. Startup applies the
   embedded sqlx migrations before binding.
4. Verify `/healthz`, `/readyz`, `/metrics`, native login, one test-mode key, one
   settled inference, and `vend-reconcile` with zero failures.
5. Deploy one production canary, verify the same probes, then roll out replicas.
6. Keep the prior image digest until reconciliation and payout lag remain zero
   for at least one complete stale-hold window.

Migrations are forward-only. Do not attempt to delete rows from
`_sqlx_migrations` or manually reverse DDL. Roll back an application image only
when the previous binary is compatible with the additive schema. Otherwise,
stop writers, restore the pre-deploy backup into a new database, verify it, and
switch `DATABASE_URL` under an incident change record.

## Backup and restore

Create a non-overwriting, owner-free custom archive:

```bash
DATABASE_URL="$DATABASE_URL" \
VEND_BACKUP_FILE="/secure-backups/vend-$(date -u +%Y%m%dT%H%M%SZ).dump" \
apps/vend/scripts/backup-postgres.sh
```

Restore only into a disposable database first. The restore command intentionally
uses `--clean`; its explicit confirmation must name the connected database:

```bash
DATABASE_URL='postgres://.../vend_restore_test' \
VEND_BACKUP_FILE='/secure-backups/vend-....dump' \
VEND_RESTORE_DATABASE='vend_restore_test' \
VEND_RESTORE_CONFIRM='restore:vend_restore_test' \
apps/vend/scripts/restore-postgres.sh
```

After restore, start Vend against the restored database with inference traffic
blocked, check `/readyz`, compare tenant/app/key/user counts, run the reconciler,
and inspect captured charge and payout totals before approving recovery.
Encrypt backups, restrict them to the database recovery role, copy them to a
second failure domain, and exercise restore quarterly. Retention and RPO/RTO are
operator policy and must be written before launch.

## Secret and session rotation

- Admin bearer: generate the replacement without changing the current bearer.
  Deploy its SHA-256 digest as `VEND_ADMIN_TOKEN_NEXT_SHA256`, roll every Vend
  replica, and verify both old and new callers. Move callers to the new bearer.
  Then promote the new digest to `VEND_ADMIN_TOKEN_SHA256`, unset
  `VEND_ADMIN_TOKEN_NEXT_SHA256`, roll again, verify the old bearer is rejected,
  and delete it. Never configure the same digest in both slots.
- Mint and Relay service keys: create a new downstream key, update the Vend
  secret, roll instances, verify readiness and one test-mode call, then revoke
  the old key downstream.
- GitHub/Google client secrets: create or reset the provider secret, update the
  Vend secret, roll instances, exercise the provider login, then revoke the old
  secret. Callback URLs do not change during secret rotation.
- Database credential: create a replacement login or password that can coexist,
  update the secret-backed `DATABASE_URL`, roll and verify every replica plus
  the reconciler, then revoke the old credential. Do not rotate by editing the
  DSN inside a running container.
- Rate-limit pepper: it has no dual-read slot. Rotating it changes subject
  digests and therefore resets active abuse buckets. Rotate only under an
  incident or controlled maintenance change, update all replicas together, and
  retain edge rate limiting while old rows expire and are cleaned up.
- Customer compromise: an owner revokes all sessions through
  `DELETE /v1/customer/users/{user_id}/sessions`; disable the user if needed.
- Provisioning key compromise: issue a replacement, deploy consumers, revoke
  the old key with `DELETE /v1/customer/keys/{key_id}`, and inspect usage.

Never log plaintext setup tokens, customer sessions, provisioning keys,
passwords, OAuth codes, or downstream service keys. Secret-bearing HTTP
responses must retain `Cache-Control: no-store` through the ingress.

## Reconciliation

Run `vend-reconcile` at least every five minutes with `concurrencyPolicy: Forbid`
per environment. Its minimum age must exceed the maximum Relay stream lifetime
by at least 60 seconds. Alert on every non-zero process exit and on any old
pending charge. Do not manually release a Mint hold until Vend’s usage row,
Mint’s authoritative hold state, and every payout leg have been inspected.

## Probes and alerts

- `/healthz` is process liveness. Do not make it dependency-aware.
- `/readyz` verifies Postgres, an authenticated Mint clearing-account read, and
  Relay health. Remove an instance from traffic on non-200.
- `/metrics` is Prometheus text with low-cardinality HTTP and settlement metrics.
  Keep it private to the metrics network; it is not an internet endpoint.

The current handler-duration family exports cumulative sum/count, not latency
buckets or quantiles. Alert on externally measured readiness and request latency
at the ingress until a histogram is added. All in-process counters reset on pod
restart, so short-window settlement alerts are an early signal, not an audit.

Install `deploy/prometheus-rules.yaml` and route page-level alerts to the money
movement on-call. Also alert from Postgres queries on stale `pending` usage,
open Mint holds older than the reconciliation window, and incomplete payout
legs; process-local counters reset on restart and cannot replace ledger checks.

## Live validation

The smoke script refuses to run unless all four real service coordinates and a
real invited customer/provisioning key are supplied:

```bash
VEND_BASE_URL='https://api.<product-domain>' \
VEND_SMOKE_DATABASE_URL='postgres://.../vend' \
MINT_BASE='https://mint.internal' \
RELAY_BASE='https://relay.internal' \
VEND_TENANT_ID='ten_...' VEND_EMAIL='ops@example.com' \
VEND_PASSWORD='...' VEND_PROVISIONING_KEY='vk_test_...' \
VEND_SMOKE_MODEL='tensorzero::model_name::openai::gpt-4o-mini' \
VEND_SMOKE_IDEMPOTENCY_KEY='release-20260822-smoke-v1' \
apps/vend/scripts/smoke-live.sh
```

Reusing that key is safe only with the unchanged model and fixed smoke body.
Use a new key when either changes. A passing run proves that specific deployed
path at that time; it is not evidence that backups, failover, or scale behavior
work unless those exercises were run separately.
