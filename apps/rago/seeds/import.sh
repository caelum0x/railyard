#!/usr/bin/env bash
#
# import.sh — load a rago catalog seed (billable metrics + plans) into a running
# rago API via the public REST surface. One seed == one consumer == one rago org.
#
# Rago is multi-tenant: the Bearer token (api_keys.value) selects the org the
# metrics/plans are created under. Give each consumer its OWN token so their
# catalogs stay isolated on the shared plane.
#
# Usage:
#   RAGO_API_KEY=<bearer> ./import.sh autogtm.json
#   RAGO_API_URL=https://rago.internal/api/v1 RAGO_API_KEY=<bearer> ./import.sh menivor.json
#
# Env:
#   RAGO_API_URL   Base URL of the rago API      (default: http://localhost:3000/api/v1)
#   RAGO_API_KEY   Bearer token for the consumer org   (required)
#
# Requires: bash, curl, jq.
set -euo pipefail

SEED="${1:-}"
RAGO_API_URL="${RAGO_API_URL:-http://localhost:3000/api/v1}"
RAGO_API_KEY="${RAGO_API_KEY:-}"

if [[ -z "$SEED" || ! -f "$SEED" ]]; then
  echo "usage: RAGO_API_KEY=<bearer> $0 <seed.json>" >&2
  exit 2
fi
if [[ -z "$RAGO_API_KEY" ]]; then
  echo "error: RAGO_API_KEY is required (Bearer token for the consumer org)" >&2
  exit 2
fi
command -v jq >/dev/null || { echo "error: jq is required" >&2; exit 2; }

auth=(-H "Authorization: Bearer ${RAGO_API_KEY}" -H "Content-Type: application/json")
consumer="$(jq -r '.consumer // "unknown"' "$SEED")"
echo "==> importing '${consumer}' seed into ${RAGO_API_URL}"

# POST helper: prints HTTP status; treats 2xx as success, 422 (already exists /
# validation) as a non-fatal skip so re-runs are safe.
post() {
  local path="$1" body="$2" label="$3"
  local code
  code="$(curl -sS -o /tmp/rago_import_resp.json -w '%{http_code}' \
    -X POST "${RAGO_API_URL}${path}" "${auth[@]}" -d "$body")"
  if [[ "$code" =~ ^2 ]]; then
    echo "    ok   ${label} (${code})"
  elif [[ "$code" == "422" ]]; then
    echo "    skip ${label} (${code} — already exists or validation): $(jq -rc . </tmp/rago_import_resp.json 2>/dev/null || true)"
  else
    echo "    FAIL ${label} (${code}): $(cat /tmp/rago_import_resp.json)" >&2
    return 1
  fi
}

# 1) billable metrics — POST /billable_metrics { "billable_metric": {...} }
jq -c '.billable_metrics[]' "$SEED" | while read -r m; do
  code="$(jq -r '.code' <<<"$m")"
  post "/billable_metrics" "$(jq -n --argjson m "$m" '{billable_metric: $m}')" "metric ${code}"
done

# 2) plans (with nested charges) — POST /plans { "plan": {...} }
jq -c '.plans[]' "$SEED" | while read -r p; do
  code="$(jq -r '.code' <<<"$p")"
  post "/plans" "$(jq -n --argjson p "$p" '{plan: $p}')" "plan ${code}"
done

echo "==> done. Verify:  curl -H 'Authorization: Bearer \$RAGO_API_KEY' ${RAGO_API_URL}/plans"
