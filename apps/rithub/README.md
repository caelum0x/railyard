# rithub — the forge (rust github)

The self-hostable collaboration forge built **on top of `rit`** ([`../rit`](../rit)):
repositories, organizations, permissions, issues, pull requests, review, protected branches,
webhooks, releases, audit logs. The `GitHub` equivalent.

## Status: deferred (gated)
Built only once `rit`'s core passes interop / crash-recovery / fuzz / concurrent-ref gates.
`rit` currently ships the loose-object core (35 tests incl. real-git interop). This is the forge's home.
