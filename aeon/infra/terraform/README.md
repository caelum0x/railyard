# infra/terraform

**SCAFFOLD.** Infrastructure-as-code for the aeonrift cloud footprint. These are
illustrative stubs, **not** applied configuration — no real providers, accounts, or
secrets. See ../../MIGRATION.md.

## Role

Declares the cloud resources the platform runs on: regions, the Kubernetes clusters that
host `services/*`, object storage / R2 buckets for replays + patch manifests, the CDN
distribution, and secret placeholders. State is remote and locked; nothing here is applied
until the reconciliation phase.

## Files

- [main.tf](./main.tf) — provider + core resources (cluster, buckets, CDN) — stub.
- [variables.tf](./variables.tf) — inputs (regions, sizes, domain) — stub.
- [outputs.tf](./outputs.tf) — exported endpoints for downstream tooling — stub.

## Laws inherited

- **No secrets in git** — every credential is a variable sourced from a secret manager.
- **No binaries in git** — buckets hold hashed blobs (replays, chunks); git holds only manifests.
- **Multi-region** — the design is region-parameterized from day one.
