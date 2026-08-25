# infra/docker

**SCAFFOLD.** Container build definitions for services + the local dev stack. Illustrative
stubs, not wired into CI. See ../../MIGRATION.md.

## Role

Defines how a Rust service crate is built into a small, pinned container image, and a
`docker-compose.yml` that stands up the backing stores (Postgres, Redis, an object-store
emulator) for local development of `services/*`.

## Files

- [Dockerfile.service](./Dockerfile.service) — multi-stage build for any `services/*` binary.
- [docker-compose.yml](./docker-compose.yml) — local dev dependencies (db, cache, object store).
- [dockerignore](./dockerignore) — build-context exclusions (rename to `.dockerignore` when applied).

## Conventions

- Multi-stage: build on the pinned toolchain (`rust-toolchain.toml`, 1.96.1), run on a distroless base.
- Images are tagged by git sha and pushed by digest; k8s pins the digest.
- No secrets baked into images — injected at runtime from the secret manager.
