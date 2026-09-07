# Mint / AITP SDKs

The stable contract is [`../contracts/aitp-0.1.schema.json`](../contracts/aitp-0.1.schema.json).

- Rust: `crates/mint-sdk` for Mint HTTP and
  `../../agent-runtime/crates/aitp-client` for signed AITP-402 retries.
- Python: `sdks/python`.
- TypeScript: `sdks/typescript`.

All three use integer microdollars, caller-provided idempotency keys, AITP 0.1
headers, the same sorted-key authorization intent, BLAKE3 operation hashes, and
Ed25519 signatures. Python and TypeScript intentionally inject the wallet/HSM
signer so private key custody stays outside application process code.

Transfer responses in every SDK include Mint's signed sender-chain receipt and
the privacy-safe `mint-transfer-attestation-v1`. Python and TypeScript validate
that the proof is present and structurally bound to the submitted sender,
amount, operation, and journal sequence. Consumers that make a cryptographic
trust decision must verify against an explicitly pinned key; the public
discovery document supports rotation but is not itself a trust root.
