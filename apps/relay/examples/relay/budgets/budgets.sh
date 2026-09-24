#!/usr/bin/env bash
# Set a monthly spend cap on an app key, spend through it, then read spend.
#
#   OPERATOR_KEY=sk-t0-... APP_KEY=sk-t0-... ./budgets.sh
#
# OPERATOR_KEY's public id must be listed in RELAY_OPERATOR_KEY_IDS on the
# gateway. Caps are in micro-USD (1,000,000 = 1 USD).
set -euo pipefail

RELAY_URL="${RELAY_URL:-http://localhost:3000}"
CAP_MICROS="${CAP_MICROS:-100}" # 0.0001 USD, low on purpose so it trips fast
: "${OPERATOR_KEY:?Set OPERATOR_KEY (sk-t0-...) }"
: "${APP_KEY:?Set APP_KEY (sk-t0-...) }"

# A key is sk-t0-<public_id>-<secret>. Caps are keyed by the public id.
APP_KEY_ID="$(cut -d- -f3 <<<"${APP_KEY}")"

echo "== set a ${CAP_MICROS} micro-USD monthly cap on key ${APP_KEY_ID}"
curl -sS -X PUT "${RELAY_URL}/relay/billing/budgets/${APP_KEY_ID}" \
  -H "Authorization: Bearer ${OPERATOR_KEY}" \
  -H "Content-Type: application/json" \
  -d "{\"monthly_limit_micros\": ${CAP_MICROS}, \"note\": \"budgets example\"}"
echo

echo "== spend with the app key until the cap trips"
for i in $(seq 1 20); do
  status="$(curl -sS -o /tmp/relay-budget-response.json -w '%{http_code}' \
    "${RELAY_URL}/openai/v1/chat/completions" \
    -H "Authorization: Bearer ${APP_KEY}" \
    -H "Content-Type: application/json" \
    -d '{"model":"tensorzero::function_name::chat","messages":[{"role":"user","content":"Write one sentence about budgets."}]}')"
  echo "request ${i}: HTTP ${status}"
  if [[ "${status}" == "429" ]]; then
    cat /tmp/relay-budget-response.json
    echo
    break
  fi
done

echo "== the app key's own month-to-date spend"
curl -sS "${RELAY_URL}/relay/billing/summary" -H "Authorization: Bearer ${APP_KEY}"
echo

echo "== every cap (operator view)"
curl -sS "${RELAY_URL}/relay/billing/budgets?scope=all" -H "Authorization: Bearer ${OPERATOR_KEY}"
echo

echo "== remove the cap"
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' -X DELETE \
  "${RELAY_URL}/relay/billing/budgets/${APP_KEY_ID}" \
  -H "Authorization: Bearer ${OPERATOR_KEY}"
