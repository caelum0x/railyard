-- Optional per-UTC-day cap on a Relay API key, next to the monthly cap. The
-- gateway sums today's `relay_cost_events.charged_micros` for the key and
-- rejects new inference once it reaches the cap, so one runaway day cannot
-- spend the whole month. NULL means no daily cap.
ALTER TABLE relay_key_budgets
    ADD COLUMN IF NOT EXISTS daily_limit_micros BIGINT
    CHECK (daily_limit_micros IS NULL OR daily_limit_micros > 0);
