#!/usr/bin/env bash
# Call Relay's OpenAI-compatible and native endpoints with curl.
# Usage: ./curl.sh            (quickstart, auth off)
#        RELAY_API_KEY=sk-t0-... ./curl.sh   (auth on)
set -euo pipefail

RELAY_URL="${RELAY_URL:-http://localhost:3000}"
AUTH=()
if [[ -n "${RELAY_API_KEY:-}" ]]; then
  AUTH=(-H "Authorization: Bearer ${RELAY_API_KEY}")
fi

echo "== health"
curl -sS "${RELAY_URL}/health"
echo

echo "== OpenAI-compatible chat completion"
curl -sS "${RELAY_URL}/openai/v1/chat/completions" \
  "${AUTH[@]+"${AUTH[@]}"}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "tensorzero::function_name::chat",
    "messages": [{"role": "user", "content": "Say hello in five words."}]
  }'
echo

echo "== native /inference"
curl -sS "${RELAY_URL}/inference" \
  "${AUTH[@]+"${AUTH[@]}"}" \
  -H "Content-Type: application/json" \
  -d '{
    "function_name": "chat",
    "input": {"messages": [{"role": "user", "content": "Name one prime number."}]}
  }'
echo
