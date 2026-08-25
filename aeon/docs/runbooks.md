# Runbooks

**SCAFFOLD.** Operational playbooks for the aeonrift platform. These are the target shape;
they fill in as `services/*` graduate from scaffold to running code.

## Conventions

- Every service exposes `/healthz` (liveness) and `/readyz` (readiness).
- Config + feature flags come from `services/liveops`; roll back a bad flag, never edit live.
- Runtime metrics: RED (rate/errors/duration) per endpoint; alerts page on SLO burn.

## Runbook: deploy a service

1. CI builds an image from `infra/docker/Dockerfile.service`, pinned by digest.
2. `infra/k8s` rolls the Deployment (surge + max-unavailable = 1); watch `/readyz`.
3. Verify RED metrics + error rate for 10 min; roll back on regression.
4. For gameservers: cordon + drain a region (`POST /v1/fleet/{region}/drain`) before rolling.

## Runbook: match server capacity shortage

- **Symptom:** `matchmaking` queue times spike; `gameservers` warm pool near zero.
- **Act:** scale the fleet Deployment (`infra/k8s/gameserver-deployment.yaml` replicas) or the
  node pool (`infra/terraform`); confirm allocations succeed via `GET /v1/fleet/{region}`.

## Runbook: economy incident (double-grant / bad price)

- Economy is a **double-entry ledger** — never hand-edit balances.
- Reverse with a compensating transaction (refund/adjustment); the ledger stays append-only.
- Freeze the offending SKU via `services/liveops` flag; publish a patch note.

## Runbook: suspected cheating wave

1. `services/anti-cheat` risk scores climb from divergence signals.
2. Pull `services/replay` evidence for flagged sessions (deterministic re-sim).
3. Issue enforcements via `POST /v1/enforcements` → `accounts-auth` applies bans.

## Runbook: content gate red on main

- A champion spec broke schema / closed-vocab / duel gate (`tests/content/`).
- Fix the spec or add the missing engine primitive to `content/_schema/vocabulary.ron`.
- Never rebaseline a golden/determinism hash to make CI pass — investigate the drift.

## Runbook: data-subject request (GDPR/COPPA)

- Export: fan-out read across accounts, economy, inventory, telemetry → packaged export.
- Erasure: delete/anonymize across owning services; ledger records archived per law.
- See `web/compliance/coppa-gdpr.md`.
