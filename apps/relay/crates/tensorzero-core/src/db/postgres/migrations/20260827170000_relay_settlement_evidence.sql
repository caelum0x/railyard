-- Principal-scoped signed settlement evidence for Relay requests. Streaming
-- responses cannot carry a receipt header produced after their headers have
-- already been sent, so consumers retrieve the immutable proof by request id.
CREATE TABLE IF NOT EXISTS relay_settlement_evidence (
    principal TEXT NOT NULL,
    request_id UUID NOT NULL,
    receipt JSONB NOT NULL,
    settled_at_ms BIGINT NOT NULL,
    PRIMARY KEY (principal, request_id)
);

CREATE INDEX IF NOT EXISTS relay_settlement_evidence_settled_at_idx
    ON relay_settlement_evidence (settled_at_ms DESC);
