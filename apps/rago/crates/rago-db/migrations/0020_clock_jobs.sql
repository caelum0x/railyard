-- Clock-jobs / billing-automation support schema.
--
-- Added by the rago-jobs clock-parity work. Kept in a single, high-numbered,
-- purely-additive migration (all `IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`)
-- so it never conflicts with in-flight rago-db/rago-billing migrations and can
-- be applied in any order relative to them.

-- Daily usage snapshots feeding analytics (Clock::ComputeAllDailyUsagesJob →
-- DailyUsages::ComputeAllService). One row per (subscription, usage day).
CREATE TABLE IF NOT EXISTS daily_usages (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    subscription_external_id varchar NOT NULL,
    usage_date date NOT NULL,
    amount_cents bigint DEFAULT 0 NOT NULL,
    refreshed_at timestamp(6) NOT NULL,
    created_at timestamp(6) NOT NULL,
    UNIQUE (organization_id, subscription_external_id, usage_date)
);
CREATE INDEX IF NOT EXISTS index_daily_usages_on_org ON daily_usages (organization_id);

-- Inbound webhooks received from payment providers (Clock::InboundWebhooksCleanupJob
-- + Clock::InboundWebhooksRetryJob → InboundWebhooks::ProcessJob).
CREATE TABLE IF NOT EXISTS inbound_webhooks (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    source varchar NOT NULL,
    event_type varchar,
    payload jsonb DEFAULT '{}' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL, -- pending | processing | succeeded | failed
    processing_attempts integer DEFAULT 0 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_inbound_webhooks_on_status ON inbound_webhooks (status);
CREATE INDEX IF NOT EXISTS index_inbound_webhooks_on_updated_at ON inbound_webhooks (updated_at);

-- Wallet recurring top-up rules gain the scheduling + lifecycle columns the
-- interval-top-up and rule-termination clocks need
-- (Clock::CreateIntervalWalletTransactionsJob + Clock::TerminateRecurringTransactionRulesJob).
ALTER TABLE wallet_recurring_rules ADD COLUMN IF NOT EXISTS status integer DEFAULT 0 NOT NULL; -- 0 active, 1 terminated
ALTER TABLE wallet_recurring_rules ADD COLUMN IF NOT EXISTS started_at timestamp(6);
ALTER TABLE wallet_recurring_rules ADD COLUMN IF NOT EXISTS next_top_up_at timestamp(6);
ALTER TABLE wallet_recurring_rules ADD COLUMN IF NOT EXISTS expiration_at timestamp(6);
ALTER TABLE wallet_recurring_rules ADD COLUMN IF NOT EXISTS terminated_at timestamp(6);
