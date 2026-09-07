-- Shared Abacus recovery state. Mint remains the money authority; this table
-- records only reservations that have not yet reached a terminal capture or
-- release, allowing any replica to recover them after process loss.
CREATE TABLE IF NOT EXISTS abacus_open_holds (
    hold_id          TEXT PRIMARY KEY,
    idem_key         TEXT NOT NULL,
    reserved_micros  BIGINT NOT NULL CHECK (reserved_micros >= 0),
    created_at_ms    BIGINT NOT NULL,
    deadline_ms      BIGINT,
    inserted_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS abacus_open_holds_created
    ON abacus_open_holds (created_at_ms, hold_id);
