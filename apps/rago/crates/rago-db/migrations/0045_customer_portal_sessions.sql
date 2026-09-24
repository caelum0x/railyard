-- Free, provider-neutral hosted customer portal (`/portal/:token`).
--
-- Every portal link is a signed token (HMAC under a key derived from the
-- organization's signing key) that names one row here. The row makes each link
-- individually revocable and lets an operator revoke every open link of a
-- customer at once; the token's own expiry is re-checked against `expires_at`.
CREATE TABLE IF NOT EXISTS customer_portal_sessions (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    expires_at timestamp(6) NOT NULL,
    revoked_at timestamp(6),
    last_used_at timestamp(6),
    created_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS index_customer_portal_sessions_on_customer
    ON customer_portal_sessions (organization_id, customer_id);
