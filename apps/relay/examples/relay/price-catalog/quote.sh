#!/usr/bin/env bash
# List the published catalog and get exact quotes from it.
# Usage: ./quote.sh                       (quickstart, auth off)
#        RELAY_API_KEY=sk-t0-... ./quote.sh   (auth on)
set -euo pipefail

RELAY_URL="${RELAY_URL:-http://localhost:3000}"
AUTH=()
if [[ -n "${RELAY_API_KEY:-}" ]]; then
  AUTH=(-H "Authorization: Bearer ${RELAY_API_KEY}")
fi

echo "== catalog (ids, prices in micro-USD per million tokens)"
curl -sS "${RELAY_URL}/v1/relay/catalog" "${AUTH[@]+"${AUTH[@]}"}" |
  python3 -c '
import json, sys
snapshot = json.load(sys.stdin)["data"]
print("revision:", snapshot["revision"])
for model in snapshot["models"]:
    mid = model["id"]
    rate_in = model["pricing"]["input_micro_usd_per_million"]
    rate_out = model["pricing"]["output_micro_usd_per_million"]
    state = "enabled" if model.get("enabled", True) else "disabled"
    print(f"  {mid:<48} in={rate_in:>9} out={rate_out:>9} {state}")
'

quote() {
  curl -sS "${RELAY_URL}/v1/relay/quotes" "${AUTH[@]+"${AUTH[@]}"}" \
    -H "Content-Type: application/json" \
    -d "{\"model\": \"$1\", \"input_tokens\": $2, \"output_tokens\": $3}"
  echo
}

echo "== quote: chat function, 1,000 input + 500 output tokens"
quote "tensorzero::function_name::chat" 1000 500

echo "== quote: the same OpenAI model called directly"
quote "tensorzero::model_name::openai::gpt-4o-mini" 1000 500

echo "== quote: a model the catalog does not publish (expect 404)"
quote "tensorzero::model_name::not-in-catalog" 1000 500
