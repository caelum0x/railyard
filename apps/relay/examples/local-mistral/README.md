# local-mistral — self-hosted inference for Relay via mistral.rs

Wires [mistral.rs](https://github.com/EricLBuehler/mistral.rs) (MIT, engine:
[candle](https://github.com/huggingface/candle)) into Relay as a self-hosted,
OpenAI-compatible provider. **Config-only — no Relay code changes.**

- `config/tensorzero.toml` — ready-to-run config (localhost default,
  `http://localhost:1234/v1/`) with two representative local models
  (`local-mistral` → `mistralai/Mistral-7B-Instruct-v0.3` via the `vllm` driver,
  `local-qwen` → `Qwen/Qwen3-4B` via the generic `openai` driver).
- `config/tensorzero.template.toml` — same config with `${MISTRAL_RS_URL}` for
  ComfyUI-style env-configurable base URL; render with `envsubst`.

Full walkthrough (launch command, GPU requirement, curl examples, tests):
[`docs/relay/LOCAL-INFERENCE.md`](../../docs/relay/LOCAL-INFERENCE.md).

## Quick start

```bash
# On a GPU host:
mistralrs serve -m Qwen/Qwen3-4B -p 1234

# Gateway (from apps/relay):
gateway --config-file examples/local-mistral/config/tensorzero.toml
```

> A GPU is required to actually serve a model. This repo's environment has none;
> config parsing and request routing are verified with an injected fake
> transport in the test suite (see the doc above).
