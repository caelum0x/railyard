#!/usr/bin/env bash
set -euo pipefail

required=(
  VEND_BASE_URL VEND_SMOKE_DATABASE_URL MINT_BASE RELAY_BASE
  VEND_TENANT_ID VEND_EMAIL VEND_PASSWORD VEND_PROVISIONING_KEY
  VEND_SMOKE_MODEL VEND_SMOKE_IDEMPOTENCY_KEY
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    echo "vend smoke: required environment variable $name is missing" >&2
    exit 64
  fi
done
for command in cmp curl jq psql; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "vend smoke: required command '$command' is unavailable" >&2
    exit 69
  fi
done

smoke_tmp="$(mktemp -d "${TMPDIR:-/tmp}/vend-smoke.XXXXXX")"
cleanup() { rm -rf "$smoke_tmp"; }
trap cleanup EXIT

probe() {
  local name="$1"
  local url="$2"
  if ! curl --fail-with-body --silent --show-error \
    --connect-timeout 5 --max-time 15 "$url" >"$smoke_tmp/$name.json"; then
    echo "vend smoke: $name is unavailable at $url" >&2
    exit 1
  fi
  echo "vend smoke: $name reachable"
}

vend_base="${VEND_BASE_URL%/}"
mint_base="${MINT_BASE%/}"
relay_base="${RELAY_BASE%/}"
probe vend-liveness "$vend_base/healthz"
probe vend-readiness "$vend_base/readyz"
probe mint-liveness "$mint_base/healthz"
probe mint-readiness "$mint_base/readyz"
probe relay-readiness "${RELAY_HEALTH_URL:-$relay_base/health}"

if ! psql "$VEND_SMOKE_DATABASE_URL" -X --set=ON_ERROR_STOP=1 \
  --tuples-only --no-align \
  --command="SELECT current_database(), count(*) FROM _sqlx_migrations WHERE success" \
  >"$smoke_tmp/postgres.txt"; then
  echo "vend smoke: Postgres is unavailable or Vend migrations are not readable" >&2
  exit 1
fi
echo "vend smoke: Postgres reachable with successful Vend migrations"

login_body="$(jq -cn \
  --arg tenant "$VEND_TENANT_ID" \
  --arg email "$VEND_EMAIL" \
  --arg password "$VEND_PASSWORD" \
  '{tenant_id:$tenant,email:$email,password:$password}')"
if ! curl --fail-with-body --silent --show-error \
  --connect-timeout 5 --max-time 30 \
  -H 'Content-Type: application/json' \
  --data-binary @- \
  "$vend_base/v1/auth/login" <<<"$login_body" >"$smoke_tmp/login.json"; then
  echo "vend smoke: native customer login failed" >&2
  exit 1
fi
customer_session="$(jq -er '.data.session_token | select(length > 20)' "$smoke_tmp/login.json")" || {
  echo "vend smoke: login did not return a session token" >&2
  exit 1
}

if ! curl --fail-with-body --silent --show-error \
  --connect-timeout 5 --max-time 15 \
  -H "Authorization: Bearer $customer_session" \
  "$vend_base/v1/customer/apps" >"$smoke_tmp/apps.json"; then
  echo "vend smoke: authenticated customer app listing failed" >&2
  exit 1
fi
jq -e '.success == true and (.data | type == "array")' "$smoke_tmp/apps.json" >/dev/null || {
  echo "vend smoke: customer app listing returned an unexpected shape" >&2
  exit 1
}

chat_body="$(jq -cn --arg model "$VEND_SMOKE_MODEL" \
  '{model:$model,messages:[{role:"user",content:"Reply with exactly: vend smoke ok"}],max_tokens:16,temperature:0,stream:false}')"
if ! curl --fail-with-body --silent --show-error \
  --connect-timeout 5 --max-time "${VEND_SMOKE_CHAT_TIMEOUT_SECONDS:-180}" \
  --dump-header "$smoke_tmp/chat.headers" \
  -H "Authorization: Bearer $VEND_PROVISIONING_KEY" \
  -H "Idempotency-Key: $VEND_SMOKE_IDEMPOTENCY_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary @- \
  "$vend_base/v1/chat/completions" <<<"$chat_body" >"$smoke_tmp/chat.json"; then
  echo "vend smoke: real metered inference failed; inspect Vend, Mint, and Relay logs" >&2
  exit 1
fi
jq -e 'type == "object" and (.choices | type == "array")' "$smoke_tmp/chat.json" >/dev/null || {
  echo "vend smoke: inference did not return an OpenAI-compatible response" >&2
  exit 1
}
charge_id="$(awk 'tolower($1) == "vend-charge-id:" {gsub("\r", "", $2); print $2}' "$smoke_tmp/chat.headers" | tail -1)"
price_micros="$(awk 'tolower($1) == "vend-price-micros:" {gsub("\r", "", $2); print $2}' "$smoke_tmp/chat.headers" | tail -1)"
if [[ ! "$charge_id" =~ ^chg_ ]] || [[ ! "$price_micros" =~ ^[0-9]+$ ]]; then
  echo "vend smoke: response is missing valid settlement headers" >&2
  exit 1
fi

# An exact retry must replay the first result rather than call Relay or move
# money again. This uses the same fixed body and caller-supplied key.
if ! curl --fail-with-body --silent --show-error \
  --connect-timeout 5 --max-time 30 \
  --dump-header "$smoke_tmp/replay.headers" \
  -H "Authorization: Bearer $VEND_PROVISIONING_KEY" \
  -H "Idempotency-Key: $VEND_SMOKE_IDEMPOTENCY_KEY" \
  -H 'Content-Type: application/json' \
  --data-binary @- \
  "$vend_base/v1/chat/completions" <<<"$chat_body" >"$smoke_tmp/replay.json"; then
  echo "vend smoke: exact idempotent replay failed" >&2
  exit 1
fi
replay_charge_id="$(awk 'tolower($1) == "vend-charge-id:" {gsub("\r", "", $2); print $2}' "$smoke_tmp/replay.headers" | tail -1)"
if [[ "$replay_charge_id" != "$charge_id" ]] || ! cmp --silent "$smoke_tmp/chat.json" "$smoke_tmp/replay.json"; then
  echo "vend smoke: retry did not replay the exact original charge and response" >&2
  exit 1
fi

if ! curl --fail-with-body --silent --show-error \
  --connect-timeout 5 --max-time 15 \
  -X POST -H "Authorization: Bearer $customer_session" \
  "$vend_base/v1/customer/logout" >/dev/null; then
  echo "vend smoke: logout failed" >&2
  exit 1
fi

echo "vend smoke: PASS charge_id=$charge_id price_micros=$price_micros"
echo "vend smoke: this exercised live Postgres, Mint, Relay, customer auth, settlement, and exact replay"
