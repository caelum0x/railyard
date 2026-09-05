-- GraphQL admin CRUD, slice 5 (final tail): orders/quotes, AI/finance
-- assistant persistence, data-export filters, and adjusted-fee revert support.
-- Faithful to Lago's quote/order model. The AI tables persist conversation +
-- message rows; the LLM call itself is owner-gated (needs a provider key).

CREATE TABLE IF NOT EXISTS quote_versions (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    quote_id uuid NOT NULL,
    version_number integer DEFAULT 1 NOT NULL,
    status varchar DEFAULT 'draft' NOT NULL,   -- draft | approved | voided
    currency varchar NOT NULL,
    sub_total_amount_cents bigint DEFAULT 0 NOT NULL,
    taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    total_amount_cents bigint DEFAULT 0 NOT NULL,
    line_items jsonb DEFAULT '[]' NOT NULL,
    expires_at timestamp(6),
    approved_at timestamp(6),
    voided_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_quote_versions_on_quote_id ON quote_versions (quote_id);

CREATE TABLE IF NOT EXISTS order_forms (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid,
    quote_id uuid,
    number varchar NOT NULL,
    status varchar DEFAULT 'draft' NOT NULL,    -- draft | signed | voided
    signed_document varchar,
    execution_mode varchar,
    execute_at timestamp(6),
    signed_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS orders (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid,
    order_form_id uuid,
    number varchar NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL,   -- pending | executed | voided
    executed_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_conversations (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    membership_id uuid,
    name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    ai_conversation_id uuid NOT NULL,
    role varchar NOT NULL,                        -- user | assistant
    content text NOT NULL,
    created_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_ai_messages_on_conversation ON ai_messages (ai_conversation_id);

CREATE TABLE IF NOT EXISTS finance_assistant_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    session_id uuid NOT NULL,
    question text NOT NULL,
    answer text,
    status varchar DEFAULT 'pending' NOT NULL,    -- pending | answered | not_configured
    created_at timestamp(6) NOT NULL
);

-- data_exports gains the resource query filters column.
ALTER TABLE data_exports ADD COLUMN IF NOT EXISTS filters jsonb DEFAULT '{}' NOT NULL;

-- adjusted_fees remembers the pre-adjustment values so a destroy can revert.
ALTER TABLE adjusted_fees ADD COLUMN IF NOT EXISTS original_units numeric;
ALTER TABLE adjusted_fees ADD COLUMN IF NOT EXISTS original_amount_cents bigint;
ALTER TABLE adjusted_fees ADD COLUMN IF NOT EXISTS original_unit_amount_cents bigint;
