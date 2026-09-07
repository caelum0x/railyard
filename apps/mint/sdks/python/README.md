# Railyard Mint Python SDK

Standard-library Mint HTTP client plus AITP-402 challenge handling for Python
3.11+. All money values are integer microdollars.

`PaidHttpClient` retries a protected request exactly once after a 402. Inject a
wallet/HSM implementing `WalletSigner`; it receives the canonical intent bytes
and must return the BLAKE3 digest plus its Ed25519 signature. Private keys never
enter the SDK.

Trusted backends can call `MintClient.transfer({...})` with `from`, `to`, a
positive integer `amount_micros`, and a stable `idem_key`. Keep the API key and
Mint account ids server-side; consumer apps should resolve public identities in
their own authenticated backend. Transfer success requires Mint's signed sender
receipt and privacy-safe attestation; the client rejects missing proof or proof
whose sender, amount, operation, or sequence differs from the request.
