# web/compliance

**SCAFFOLD.** ereboslab.com legal/compliance surface. Not a compiled crate; not a Cargo
workspace member. MIGRATE: ereboslab.com scaffold (see ../../MIGRATION.md).

## Role

Hosts the legally required documents and the machinery that backs them: privacy policy,
terms of service, and the age-gate / data-subject-rights notes that pair with the consent
records owned by `services/accounts-auth`. These are **drafts/scaffolds** — not legal advice
and not final copy; real text is reviewed by counsel before publication.

## Files

- [privacy-policy.md](./privacy-policy.md) — data collected, purpose, retention, contacts.
- [terms-of-service.md](./terms-of-service.md) — account, conduct, and license terms.
- [coppa-gdpr.md](./coppa-gdpr.md) — age gate, consent, and data-subject-rights flows.

## Ties to services

- Age gate + consent capture → `services/accounts-auth` (`POST /v1/accounts/{id}/consent`).
- Data export / deletion requests → fan-out across accounts, economy, inventory, telemetry.
- Telemetry carries only a hashed account id (no raw PII), consistent with the privacy policy.
