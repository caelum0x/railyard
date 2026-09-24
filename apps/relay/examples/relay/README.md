# Relay examples

Each example builds on the quickstart in the repository root
(`docker-compose.quickstart.yml`). Start there, then pick one:

| Example | Shows | Needs |
| --- | --- | --- |
| [`openai-client`](openai-client) | Calling Relay from the OpenAI Python and Node SDKs and curl: functions, named models, streaming, embeddings. | `OPENAI_API_KEY` |
| [`fallback-routing`](fallback-routing) | Retries, provider failover, fallback variants across model families, optional latency hedging. | `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` |
| [`budgets`](budgets) | API keys, per-request cost recording, and per-key monthly spend caps that return `429` when reached. | `OPENAI_API_KEY`, Docker for Postgres |
| [`price-catalog`](price-catalog) | Publishing your own prices, exact quotes, and keeping list prices current. | `OPENAI_API_KEY` |

The other folders in `examples/` come from TensorZero, the project Relay is
forked from. They still apply to Relay because the gateway, config format and
API are unchanged, but they reference TensorZero's published images
(`tensorzero/gateway`, `tensorzero/ui`). To use Relay instead, build this
repository's `Dockerfile` and change the `image:` lines.
