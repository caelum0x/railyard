# Prism — universal media-generation gateway

Prism Sx0 is now an executable FastAPI service over ReelForge's reviewed media
catalog and routing policy. It exposes image and video models through one
OpenAI-shaped surface, performs consented equivalent-model failover, supports a
hard microdollar cost cap, polls queued video providers, and provides owner-scoped
exact-cost readback.

Implemented endpoints:

- `GET /v1/models?modality=image|video`
- `POST /v1/media/generations`
- `GET /v1/generation?id=gen_...`
- `GET /healthz`

Authentication fails closed. Set `PRISM_API_KEY_SHA256S` to comma-separated
SHA-256 hashes of accepted Prism keys. Callers may send a key as
`Authorization: Bearer ...` or `X-Prism-Key`. Optional BYOK credentials use
`X-Provider` plus `X-Provider-Api-Key` and are never persisted. For asynchronous
video, resend those two BYOK headers while polling `GET /v1/generation`; Prism
uses the credential for that request only.

```bash
cd apps/prism-media-gateway
PRISM_API_KEY_SHA256S="$(printf prism-dev | shasum -a 256 | cut -d' ' -f1)" \
PYTHONPATH=../api:. ../../.venv/bin/python -m uvicorn prism_gateway.app:app --port 8090

PYTHONPATH=../api:. ../../.venv/bin/python -m pytest tests -q
```

The Sx0 readback store is deliberately process-local and bounded. Durable jobs,
wallet hold/settle/void, arbitrage accounting, and the public cost/health
leaderboard are Sx1 work. The product roadmap remains in
[`docs/apps-vision/prism-media-gateway.md`](../../docs/apps-vision/prism-media-gateway.md).
