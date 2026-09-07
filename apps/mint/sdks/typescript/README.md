# Railyard Mint TypeScript SDK

Dependency-free Mint HTTP client and AITP-402 challenge handler for Node 20+
and modern browsers. Money is represented only as integer microdollars.

`PaidHttpClient` signs and retries one 402 while preserving the request body.
Inject a `WalletSigner` backed by your wallet, HSM, or secure enclave; it receives
canonical intent bytes and returns the BLAKE3 digest plus Ed25519 signature.
Private keys are not stored by this package.

Trusted backends can call `MintClient.transfer(...)` with a typed request and a
stable `idem_key`. The method rejects floats, unsafe integers, empty accounts,
and empty idempotency keys before transport. It also requires the signed receipt
and transfer attestation in the success response and binds their sender, amount,
operation, and sequence to the request. Never ship the Mint API key or raw
account ids to a browser or mobile binary.
