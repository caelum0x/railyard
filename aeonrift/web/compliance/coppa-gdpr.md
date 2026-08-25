# Age Gate, COPPA & GDPR (DRAFT / SCAFFOLD)

> **Not legal advice, not final copy.** Placeholder to be reviewed by counsel before publication.

## Age gate

At registration `services/accounts-auth` collects date of birth and derives an age bracket.
Accounts below the regional minimum age are blocked or routed to a
parental-consent flow before any account is created.

## COPPA (US, under-13)

- No account creation without verifiable parental consent.
- No behavioral profiling or targeted offers for known-child accounts.
- Consent decisions recorded via `POST /v1/accounts/{id}/consent`.

## GDPR / data-subject rights (EU/UK)

| Right | Flow |
|-------|------|
| Access / export | Fan-out read across accounts, economy, inventory, telemetry → packaged export |
| Rectification | Update via account settings → accounts-auth |
| Erasure | Delete/anonymize across all owning services; ledger records archived per law |
| Consent withdrawal | Re-prompt; disable optional processing |

## Data minimization

Telemetry stores only a **hashed** account id — raw PII never enters the analytics path,
consistent with [privacy-policy.md](./privacy-policy.md).

## Records

Every consent/withdrawal is an immutable, timestamped record (append-only), so the
compliance state of any account is reconstructable.
