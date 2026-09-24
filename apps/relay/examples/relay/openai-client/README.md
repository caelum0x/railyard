# OpenAI-compatible client

Relay serves the OpenAI Chat Completions and Embeddings APIs under
`/openai/v1`. Any OpenAI SDK works by changing the base URL.

```bash
# From the repository root
export OPENAI_API_KEY=sk-...
docker compose -f docker-compose.quickstart.yml up --build -d

cd examples/relay/openai-client
./curl.sh                              # no dependencies
pip install openai && python chat.py   # Python SDK
npm install openai && node chat.mjs    # Node SDK
```

## Choosing what to call

The `model` field selects a Relay route:

| `model` value | What it calls |
| --- | --- |
| `tensorzero::function_name::chat` | The `chat` function in `tensorzero.toml`. The config decides which model runs, so you can switch models without a client change. |
| `tensorzero::model_name::gpt-4o-mini` | A named model from the config, with its provider routing and failover. |
| `tensorzero::model_name::openai::gpt-4o-mini` | A provider model used directly, without a config entry. The gateway still needs that provider's key. |
| `tensorzero::embedding_model_name::text-embedding-3-small` | A named embedding model. |

These selector names are inherited from TensorZero and are kept unchanged so
existing TensorZero clients keep working against Relay.

## Authentication

The quickstart runs with auth off, so the SDK's `api_key` can be any string.
With `[gateway.auth] enabled = true`, create a key with
`gateway --create-api-key` (it prints `sk-t0-<public_id>-<secret>`) and pass
it as the SDK `api_key`, or set `RELAY_API_KEY` for these scripts.
