#!/usr/bin/env bash
#
# provision-relay-subscriptions.sh — create the Rago customer + subscriptions
# that Relay's cost feed needs before RELAY_COST_FEED_FORMAT=rago can deliver.
#
# The cost feed (apps/relay/crates/gateway/src/rago_feed.rs) posts each
# metered inference to Rago as a usage event against
# `external_subscription_id = subscription_for(owner_principal)`
# (default: `RELAY_RAGO_SUBSCRIPTION_PREFIX + <relay API key public id>`, or an
# explicit `RELAY_RAGO_SUBSCRIPTION_MAP`/`RELAY_RAGO_ROUTES_JSON` entry — see
# apps/relay/crates/gateway/src/rago_feed.rs module docs). Rago rejects an
# event for a subscription that does not exist, so every subscription id the
# feed will use must be created here FIRST, or the event is retried and then
# dead-lettered (never billed to the wrong org — see relay-cost-feed's
# `CostFeedWorker`, but never delivered either).
#
# This script is idempotent: customer creation is an upsert on the Rago side
# (handlers::create_customer), and each subscription is created only if a
# GET on its external_id 404s.
#
# Usage:
#   RAGO_API_KEY='rago_live_...' ./provision-relay-subscriptions.sh \
#     --customer autogtm-relay-portfolio \
#     --name "Railyard Relay LLM (portfolio shadow mirror)" \
#     --plan gtm_usage \
#     --prefix relay_ \
#     <relay-api-key-public-id> [<relay-api-key-public-id> ...]
#
# Env:
#   RAGO_API_URL   Base URL of the rago API      (default: http://localhost:3000/api/v1)
#   RAGO_API_KEY   Bearer token for the consumer org   (required)
#
# Requires: bash, curl, jq.

set -euo pipefail

RAGO_API_URL="${RAGO_API_URL:-http://localhost:3000/api/v1}"
RAGO_API_KEY="${RAGO_API_KEY:-}"

customer_external_id=""
customer_name=""
plan_code=""
prefix=""
declare -a keys=()

usage() {
  echo "usage: RAGO_API_KEY=<bearer> $0 --customer <external_id> --name <name> --plan <plan_code> --prefix <subscription_prefix> <relay-key-public-id> [...]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --customer) customer_external_id="$2"; shift 2 ;;
    --name) customer_name="$2"; shift 2 ;;
    --plan) plan_code="$2"; shift 2 ;;
    --prefix) prefix="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) keys+=("$1"); shift ;;
  esac
done

[[ -n "$customer_external_id" && -n "$plan_code" && ${#keys[@]} -gt 0 ]] || usage
[[ -n "$RAGO_API_KEY" ]] || { echo "error: RAGO_API_KEY is required (Bearer token for the consumer org)" >&2; exit 2; }
command -v jq >/dev/null || { echo "error: jq is required" >&2; exit 2; }

auth=(-H "Authorization: Bearer ${RAGO_API_KEY}" -H "Content-Type: application/json")

echo "==> upserting customer '${customer_external_id}' in ${RAGO_API_URL}"
customer_body="$(jq -n --arg id "$customer_external_id" --arg name "$customer_name" \
  '{customer: ({external_id: $id} + (if $name == "" then {} else {name: $name} end))}')"
code="$(curl -sS -o /tmp/rago_provision_resp.json -w '%{http_code}' \
  -X POST "${RAGO_API_URL}/customers" "${auth[@]}" -d "$customer_body")"
if [[ "$code" =~ ^2 ]]; then
  echo "    ok   customer ${customer_external_id} (${code})"
else
  echo "    FAIL customer ${customer_external_id} (${code}): $(cat /tmp/rago_provision_resp.json)" >&2
  exit 1
fi

for key in "${keys[@]}"; do
  sub_external_id="${prefix}${key}"
  show_code="$(curl -sS -o /tmp/rago_provision_show.json -w '%{http_code}' "${RAGO_API_URL}/subscriptions/${sub_external_id}" "${auth[@]}")"
  if [[ "$show_code" == "200" ]]; then
    echo "    skip subscription ${sub_external_id} (already exists)"
    continue
  fi
  sub_body="$(jq -n --arg cust "$customer_external_id" --arg plan "$plan_code" --arg id "$sub_external_id" \
    '{subscription: {external_customer_id: $cust, plan_code: $plan, external_id: $id}}')"
  create_code="$(curl -sS -o /tmp/rago_provision_resp.json -w '%{http_code}' \
    -X POST "${RAGO_API_URL}/subscriptions" "${auth[@]}" -d "$sub_body")"
  if [[ "$create_code" =~ ^2 ]]; then
    echo "    ok   subscription ${sub_external_id} (${create_code})"
  else
    echo "    FAIL subscription ${sub_external_id} (${create_code}): $(cat /tmp/rago_provision_resp.json)" >&2
    exit 1
  fi
done

echo "==> done. Verify:  curl -H 'Authorization: Bearer \$RAGO_API_KEY' ${RAGO_API_URL}/subscriptions/${prefix}<key>"
