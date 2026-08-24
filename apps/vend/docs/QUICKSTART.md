# Vend live quickstart

This path uses live Vend, Mint, Relay, and Postgres services. It does not start
or substitute mocks.

## 1. Configure an undecided product domain

Use one temporary HTTPS API origin until the final name is chosen:

```bash
export VEND_BASE_URL='https://api.<product-domain>'
export VEND_PUBLIC_BASE_URL="$VEND_BASE_URL"
```

Register these exact provider callbacks:

- GitHub OAuth App: `https://api.<product-domain>/v1/auth/github/callback`
- Google OAuth web client: `https://api.<product-domain>/v1/auth/google/callback`

Set `VEND_GITHUB_CLIENT_ID` plus `VEND_GITHUB_CLIENT_SECRET`, and/or
`VEND_GOOGLE_CLIENT_ID` plus `VEND_GOOGLE_CLIENT_SECRET`. Register the exact
production origin; do not use wildcard callback URLs. Local development uses
`http://127.0.0.1:8090` and its matching callbacks. Changing the domain is a
configuration and provider-registration change, not an identity migration.

## 2. Activate and sign in an invited owner

```bash
jq -cn --arg token "$OWNER_SETUP_TOKEN" --arg password "$VEND_PASSWORD" \
  '{setup_token:$token,password:$password}' \
  | curl --fail-with-body "$VEND_BASE_URL/v1/auth/setup" \
      -H 'Content-Type: application/json' --data-binary @-

login_json="$(jq -cn --arg tenant "$VEND_TENANT_ID" --arg email "$VEND_EMAIL" --arg password "$VEND_PASSWORD" \
  '{tenant_id:$tenant,email:$email,password:$password}' \
  | curl --fail-with-body "$VEND_BASE_URL/v1/auth/login" \
      -H 'Content-Type: application/json' --data-binary @-)"
export VEND_CUSTOMER_SESSION="$(jq -er '.data.session_token' <<<"$login_json")"
unset login_json
```

For GitHub or Google login, send the invited user’s browser to one of these
URLs. The provider email must be verified and match the existing invitation:

```text
https://api.<product-domain>/v1/auth/github/start?tenant_id=<tenant-id>
https://api.<product-domain>/v1/auth/google/start?tenant_id=<tenant-id>
```

## 3. Create an app and issue its one-time key

```bash
app_json="$(jq -cn --arg mint "$VEND_MINT_ACCOUNT_ID" --arg model "$VEND_MODEL" '{
    name:"Production assistant",
    mint_account_id:$mint,
    model_allowlist:[$model],
    zdr:false,
    markup_policy:{markup_bps:2000,min_fee_micros:0,platform_fee_bps:1000,funding_mode:"tenant_reserve"}
  }' | curl --fail-with-body "$VEND_BASE_URL/v1/customer/apps" \
    -H "Authorization: Bearer $VEND_CUSTOMER_SESSION" \
    -H 'Content-Type: application/json' --data-binary @-)"
export VEND_APP_ID="$(jq -er '.data.id' <<<"$app_json")"
unset app_json

key_json="$(printf %s '{"environment":"live","daily_cap_micros":50000000,"per_end_user_daily_cap_micros":5000000}' \
  | curl --fail-with-body "$VEND_BASE_URL/v1/customer/apps/$VEND_APP_ID/keys" \
  -H "Authorization: Bearer $VEND_CUSTOMER_SESSION" \
  -H 'Content-Type: application/json' \
  --data-binary @-)"
export VEND_PROVISIONING_KEY="$(jq -er '.data.secret' <<<"$key_json")"
unset key_json
```

Move `VEND_PROVISIONING_KEY` into a secret manager immediately. Vend returns it
once; key-list APIs expose only safe metadata.

## 4. Call non-streaming and streaming inference

```bash
curl --fail-with-body --dump-header /tmp/vend-headers \
  "$VEND_BASE_URL/v1/chat/completions" \
  -H "Authorization: Bearer $VEND_PROVISIONING_KEY" \
  -H "Idempotency-Key: $(openssl rand -hex 16)" \
  -H 'Content-Type: application/json' \
  --data "$(jq -cn --arg model "$VEND_MODEL" '{
    model:$model,messages:[{role:"user",content:"Hello"}],max_tokens:256,stream:false
  }')"
grep -Ei '^Vend-(Charge-Id|Base-Cost-Micros|Price-Micros|Model):' /tmp/vend-headers
rm -f /tmp/vend-headers

curl --fail-with-body --no-buffer \
  "$VEND_BASE_URL/v1/chat/completions" \
  -H "Authorization: Bearer $VEND_PROVISIONING_KEY" \
  -H "Idempotency-Key: $(openssl rand -hex 16)" \
  -H 'Content-Type: application/json' \
  --data "$(jq -cn --arg model "$VEND_MODEL" '{
    model:$model,messages:[{role:"user",content:"Hello"}],max_tokens:256,stream:true
  }')"
```

Every retry of a non-streaming call must reuse both the exact JSON body and the
same idempotency key. A streaming key is single-use and its emitted bytes are
not replayed.
