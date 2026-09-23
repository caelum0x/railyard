# turnstile-client (Python)

Agent-side AITP-402 payment client for Turnstile-gated HTTP endpoints and MCP
tools. It needs only the standard library (`urllib`, a bundled BLAKE3 and a
bundled RFC 8032 Ed25519). Installing the optional `fast` extra
(`cryptography`, `blake3`) switches to constant-time Ed25519 and native BLAKE3.
Requires Python 3.9 or newer.

```python
import os
from turnstile_client import Ed25519Signer, SpendPolicy, TurnstileClient

signer = Ed25519Signer.from_seed_hex("key_agent", os.environ["AGENT_ED25519_SEED_HEX"])
# Register signer.public_key_hex with Mint under (payer, "key_agent") once.

client = TurnstileClient(
    base_url="https://tools.example.com",
    payer="acct_agent",
    signer=signer,
    policy=SpendPolicy(max_price_micros=50_000, total_budget_micros=5_000_000),
)

catalog = client.tools()                              # GET /v1/tools (free)
res = client.call_tool("summarize", {"text": "..."})
print(res.status, res.receipt and res.receipt["amount_micros"])

if res.settlement == "capture_pending":
    print(client.wait_for_settlement(res)["phase"])   # GET /v1/settlements/:id

mcp = client.mcp_call("summarize", {"text": "..."})   # POST /mcp tools/call
```

The payment flow is the same as the TypeScript client: when a challenge
breaks the `SpendPolicy`, the client raises `PaymentRefused` and signs
nothing. When Turnstile rejects a signed authorization, it raises
`PaymentFailed` with the server's `AITP-Error` reason. The client never talks
to Mint directly: Mint opens the hold when Turnstile verifies the
authorization.

The pure-Python Ed25519 path is not constant-time. On shared hosts, install
`turnstile-client[fast]` or pass your own `Signer` (anything with `pubkey_id`
and `sign(op_hash) -> bytes`), for example one backed by a KMS.
