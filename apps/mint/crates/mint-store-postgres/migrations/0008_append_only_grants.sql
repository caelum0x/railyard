-- 0008_append_only_grants.sql — least-privilege app role + append-only defense.
--
-- journal / receipts / reserve_ledger are APPEND-ONLY: corrections are made with
-- compensating entries, never UPDATE/DELETE. We enforce this two ways:
--   1) a least-privilege `mint_app` role with no UPDATE/DELETE on those tables;
--   2) BEFORE UPDATE/DELETE triggers as defense-in-depth (fire even for owners).
--
-- Wrapped in a DO block so re-running (sqlx tracks the version) is a clean no-op.

DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'mint_app') THEN
        CREATE ROLE mint_app NOLOGIN;
    END IF;
END
$$;

-- Append-only tables: SELECT + INSERT only.
GRANT SELECT, INSERT ON journal        TO mint_app;
GRANT SELECT, INSERT ON receipts       TO mint_app;
GRANT SELECT, INSERT ON reserve_ledger TO mint_app;
GRANT USAGE, SELECT ON SEQUENCE journal_seq_seq        TO mint_app;
GRANT USAGE, SELECT ON SEQUENCE reserve_ledger_seq_seq TO mint_app;
GRANT USAGE, SELECT ON SEQUENCE receipts_global_seq_seq TO mint_app;

-- Mutable projection / lifecycle tables: full DML.
GRANT SELECT, INSERT, UPDATE, DELETE ON accounts    TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON account_keys TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON addresses   TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON balances    TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON reserve     TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON holds       TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON streams     TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON allowances  TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON deposits    TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON nonces      TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON idempotency TO mint_app;

-- Defense-in-depth: reject UPDATE/DELETE on append-only tables at the DB level.
CREATE OR REPLACE FUNCTION mint_reject_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'append-only table %: UPDATE/DELETE forbidden (use a compensating entry)', TG_TABLE_NAME;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS journal_append_only ON journal;
CREATE TRIGGER journal_append_only
    BEFORE UPDATE OR DELETE ON journal
    FOR EACH ROW EXECUTE FUNCTION mint_reject_mutation();

DROP TRIGGER IF EXISTS receipts_append_only ON receipts;
CREATE TRIGGER receipts_append_only
    BEFORE UPDATE OR DELETE ON receipts
    FOR EACH ROW EXECUTE FUNCTION mint_reject_mutation();

DROP TRIGGER IF EXISTS reserve_ledger_append_only ON reserve_ledger;
CREATE TRIGGER reserve_ledger_append_only
    BEFORE UPDATE OR DELETE ON reserve_ledger
    FOR EACH ROW EXECUTE FUNCTION mint_reject_mutation();
