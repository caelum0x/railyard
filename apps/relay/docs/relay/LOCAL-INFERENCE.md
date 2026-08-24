# Local self-hosted inference for Relay (mistral.rs + candle)

Relay (the TensorZero-based gateway under `apps/relay`) can route inference to a
**local, self-hosted** LLM server instead of a hosted API. This document wires
in [**mistral.rs**](https://github.com/EricLBuehler/mistral.rs) (MIT), whose
inference engine is [**candle**](https://github.com/huggingface/candle)
(Apache-2.0). Both are already vendored under `external/mistral.rs` and
`external/candle`.

Because mistral.rs exposes an **OpenAI-compatible HTTP API**, no changes to
Relay's Rust provider code are required. It is wired in purely through a config
file, using TensorZero's existing self-hosted provider kinds.

---

## How the pieces fit

```
  Relay gateway  ──HTTP POST /v1/chat/completions──►  mistralrs serve  ──►  candle
  (apps/relay)        (OpenAI-compatible)               (HTTP server)      (inference
                                                                            engine, GPU)
```

- **candle** is mistral.rs's internal inference engine (tensor ops, model
  loading, GPU kernels via CUDA/Metal). It is **not** wired into Relay directly;
  it runs *underneath* the mistral.rs HTTP server.
- **mistral.rs** loads the model with candle and serves the OpenAI contract:
  `POST /v1/chat/completions`, `POST /v1/completions`, `GET /v1/models`, etc.
  (default bind `0.0.0.0:1234`).
- **Relay** treats it as any other OpenAI-compatible provider: a `vllm` (or
  generic `openai`) provider block whose `api_base` points at the mistral.rs
  server.

---

## 1. Launch mistral.rs (requires a GPU)

Serving a real model needs a GPU (CUDA on NVIDIA, Metal on Apple Silicon). On
the GPU host:

```bash
# Build/install mistralrs with the right accelerator feature, then:
mistralrs serve -m Qwen/Qwen3-4B -p 1234
# or another served model id, e.g.:
mistralrs serve -m mistralai/Mistral-7B-Instruct-v0.3 -p 1234
```

Useful flags (see `external/mistral.rs/mistralrs-cli/src/args/`):

| Flag            | Meaning                                  | Default     |
| --------------- | ---------------------------------------- | ----------- |
| `-m/--model-id` | HF repo id or local path to serve        | (required)  |
| `-p/--port`     | HTTP port                                | `1234`      |
| `--host`        | Bind address                             | `0.0.0.0`   |
| `--no-ui`       | Disable the built-in web UI              | UI enabled  |

Confirm the served model id (Relay's `model_name` must match it exactly):

```bash
curl http://localhost:1234/v1/models
```

> **No GPU here.** This repository/environment has no GPU and cannot serve a
> real model. Everything below (config parsing + request routing) is verified
> with an injected fake transport in the test suite; actually serving weights is
> deferred to a GPU host.

## 2. Point Relay at it (config)

Ready-to-run config (localhost default) lives at
[`examples/local-mistral/config/tensorzero.toml`](../../examples/local-mistral/config/tensorzero.toml):

```toml
[models.local-mistral]
routing = ["mistralrs"]

[models.local-mistral.providers.mistralrs]
type = "vllm"                                      # OpenAI-compatible self-hosted driver
api_base = "http://localhost:1234/v1/"             # trailing slash required
model_name = "mistralai/Mistral-7B-Instruct-v0.3"  # must match GET /v1/models
api_key_location = "none"                          # mistral.rs is keyless by default

[functions.generate]
type = "chat"

[functions.generate.variants.local_mistral]
type = "chat_completion"
model = "local-mistral"
```

A second model (`local-qwen`) in the same file shows the equivalent generic
`openai` driver against the same server.

**`api_base` gotcha:** it MUST end at `/v1/` (trailing slash) and MUST NOT
include `/chat/completions` — the gateway appends that itself. The mistral.rs
default port is `1234`.

### Env-configurable base URL (ComfyUI-style)

TensorZero TOML values are literals (there is no built-in `${VAR}` interpolation
for `api_base`). To keep the base URL env-configurable with a default, render
[`tensorzero.template.toml`](../../examples/local-mistral/config/tensorzero.template.toml)
with `envsubst`:

```bash
export MISTRAL_RS_URL="${MISTRAL_RS_URL:-http://localhost:1234/v1/}"
envsubst < examples/local-mistral/config/tensorzero.template.toml \
    > examples/local-mistral/config/tensorzero.rendered.toml
```

Common values:

| Scenario                                   | `MISTRAL_RS_URL`                          |
| ------------------------------------------ | ----------------------------------------- |
| Bare-metal gateway, local server           | `http://localhost:1234/v1/` (default)     |
| Dockerized gateway, host-run server        | `http://host.docker.internal:1234/v1/`    |
| Remote GPU host                            | `http://gpu-host.internal:1234/v1/`       |

## 3. Run the gateway

```bash
cd apps/relay
gateway --config-file examples/local-mistral/config/tensorzero.toml
# or the rendered template:
# gateway --config-file examples/local-mistral/config/tensorzero.rendered.toml
```

## 4. Call it

Through the function (recommended):

```bash
curl -sS http://localhost:3000/openai/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "tensorzero::function_name::generate",
    "messages": [{"role": "user", "content": "Say hello"}]
  }'
```

Or directly by model name: `"model": "tensorzero::model_name::local-mistral"`.

---

## Verification in this environment (no GPU / no live model)

Two tests exercise the wiring without any network server or GPU:

- **Config parses + routes** —
  `crates/tensorzero-core/src/config/tests.rs::test_local_mistral_config_parses_and_routes_to_local_provider`
  loads the exact shipped config and asserts it produces a `vllm` provider
  (and an `openai` provider) pointing at the mistral.rs `api_base`, with the
  `generate` function routed to `local-mistral`.
- **Chat request via injected fake transport** —
  `crates/tensorzero-core/src/providers/vllm.rs::test_vllm_infer_routes_to_injected_fake_transport`
  spins up an in-process stub returning a canned OpenAI chat-completion and runs
  a real inference through the `vllm` provider, asserting the response is parsed
  back into TensorZero content blocks. No live server, no GPU, no weights.

Run them from the workspace root (`apps/relay/crates`):

```bash
cd apps/relay/crates
cargo test -p tensorzero-core test_local_mistral_config_parses_and_routes_to_local_provider
cargo test -p tensorzero-core test_vllm_infer_routes_to_injected_fake_transport
```

## Why config-only (no new provider code)

mistral.rs already speaks the OpenAI chat-completions protocol that TensorZero's
`vllm`/`openai`/`sglang`/`tgi` drivers target. A genuinely new provider *kind*
would only be needed for a non-OpenAI-compatible protocol — not the case here.
The `ProviderConfig` enum in `crates/tensorzero-core/src/model.rs` and the
`VLLMProvider` in `crates/tensorzero-core/src/providers/vllm.rs` already accept a
custom `api_base`, so wiring mistral.rs in is purely the config above.
