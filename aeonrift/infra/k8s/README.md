# infra/k8s

**SCAFFOLD.** Kubernetes manifests for `services/*` and the gameserver fleet. Illustrative
stubs, not applied. See ../../MIGRATION.md.

## Role

Declares how services and match servers run in-cluster: a namespace, long-running service
Deployments (matchmaking, economy, ...), and the elastic, allocation-driven gameserver
fleet (Agones-style) that `services/gameservers` scales up and down.

## Files

- [namespace.yaml](./namespace.yaml) — the `aeonrift` namespace.
- [matchmaking-service.yaml](./matchmaking-service.yaml) — a representative stateless service Deployment + Service.
- [gameserver-deployment.yaml](./gameserver-deployment.yaml) — the authoritative match-server fleet Deployment.

## Conventions

- Every workload exposes `/healthz` (liveness) and `/readyz` (readiness).
- Config comes from `services/liveops`; secrets from the cluster secret manager (never inline).
- Images are pinned by digest, built from `infra/docker`.
