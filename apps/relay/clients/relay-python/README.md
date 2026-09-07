# railyard-relay

Dependency-free Python 3.11+ SDK for Relay’s OpenAI-compatible and native
inference surfaces, AITP paid retries, settlement evidence, catalog/quotes,
health, and cost-event operations.

```python
from relay_sdk import RelayClient

relay = RelayClient(
    "https://relay.example",
    api_key="...",
    settlement_evidence_verifier=lambda evidence: verify_mint_attestation(
        evidence, pinned_mint_public_key
    ),
)
result = relay.paid_chat_completions(
    {
        "model": "tensorzero::model_name::openai::gpt-4o",
        "messages": [{"role": "user", "content": "Hello"}],
    },
    authorizer=lambda payment: wallet.authorize_aitp(payment),
)
```

The SDK uses the Python standard library by default and accepts injectable sync
or async transports. Paid methods retry exactly once. Dropped streams hand the
request UUID to an application-owned recovery queue; asynchronous settlement
lookup is deliberately separate from object finalization.
`settlement_evidence()` verifies by default; only verifier-owning recovery code
should use `settlement_evidence_unverified()`.

Settlement replay fails closed: `RelayAlreadySettledError` is emitted only when
the configured verifier approves Mint's signed attestation. Missing verification
raises `RelayUnverifiedSettlementEvidenceError`; a bad proof raises
`RelayInvalidSettlementEvidenceError`. Keep pinned Mint keys outside the
evidence envelope.

Binary multipart is available on both clients and reuses the exact encoded body
after an AITP challenge:

```python
from relay_sdk import MultipartForm

form = MultipartForm().file(
    "file", "requests.jsonl", "application/jsonl", payload_bytes
)
result = relay.paid_multipart("v1/files", form, authorizer)
```

Run `python -m pytest` and `python -m compileall -q src tests` locally.
