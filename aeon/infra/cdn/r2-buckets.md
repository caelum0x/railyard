# R2 / object-store bucket layout — cdn

**SCAFFOLD.** Reference for the buckets the CDN fronts and the services that write them.

## Buckets

| Bucket | Written by | Read by | Contents | Lifecycle |
|--------|-----------|---------|----------|-----------|
| `aeonrift-patch-chunks-<env>` | patcher (publish) | client via CDN | blake3-named immutable chunks | never deleted (immutable) |
| `aeonrift-replays-<env>` | replay (finalize) | replay, anti-cheat | match input logs (few KB each) | archived after N days |
| `aeonrift-web-<env>` | web build | crawlers, players via CDN | champion pages, store art | rebuilt each deploy |
| `aeonrift-exports-<env>` | compliance flow | data-subject | GDPR export packages | expire after 30 days |

## Access model

- **Immutable buckets** (patch chunks): write-once, public-read via CDN only, no overwrite.
- **Private buckets** (replays, exports): no public access; served through the owning service.
- Keys in patch/replay buckets are the content hash → dedupe + no-binaries-in-git are automatic.

## Naming

`<name>-<environment>` where environment ∈ {dev, staging, prod}, provisioned in
[../terraform/main.tf](../terraform/main.tf).
