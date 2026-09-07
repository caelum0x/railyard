#!/usr/bin/env python3
"""Run Relay's durable Postgres invariants against an ephemeral local cluster.

The harness uses only Python's standard library and the Postgres command-line
tools. It does not need Docker, network access, provider keys, or Mint keys.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = ROOT / "crates/tensorzero-core/src/db/postgres/migrations"
RELAY_MIGRATIONS = (
    "20260827170000_relay_settlement_evidence.sql",
    "20260828120000_relay_billing_outbox.sql",
    "20260828180000_relay_batch_billing.sql",
    "20260828190000_relay_provider_health.sql",
    "20260828200000_relay_cost_events.sql",
    "20260828201000_relay_aitp_challenges.sql",
)
REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"
SECOND_REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000003"
THIRD_REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000004"
EVENT_ID = "018f7e23-7b55-7d9a-8000-000000000102"
SECOND_EVENT_ID = "018f7e23-7b55-7d9a-8000-000000000103"
THIRD_EVENT_ID = "018f7e23-7b55-7d9a-8000-000000000104"


class SmokeFailure(RuntimeError):
    pass


def binary(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        raise SmokeFailure(f"required Postgres executable is missing: {name}")
    return path


def free_port() -> int:
    # The managed development sandbox forbids opening TCP sockets. Postgres
    # still requires a port number to name its Unix-domain socket, so derive a
    # high, process-specific value and disable TCP listening entirely.
    return 50_000 + (os.getpid() % 10_000)


def run(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    if check and result.returncode != 0:
        raise SmokeFailure(
            f"command failed ({result.returncode}): {' '.join(command)}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


class EphemeralPostgres:
    def __init__(self, root: Path) -> None:
        self.data = root / "data"
        self.socket_dir = root / "socket"
        self.log = root / "postgres.log"
        self.port = free_port()
        self.pg_ctl = binary("pg_ctl")
        self.psql_binary = binary("psql")
        self.started = False

    def start(self) -> None:
        self.socket_dir.mkdir()
        run([binary("initdb"), "-D", str(self.data), "--auth=trust", "--no-locale"])
        options = f"-p {self.port} -k {self.socket_dir} -F -c listen_addresses=''"
        run([self.pg_ctl, "-D", str(self.data), "-l", str(self.log), "-o", options, "start"])
        self.started = True
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            probe = run(
                [binary("pg_isready"), "-h", str(self.socket_dir), "-p", str(self.port)],
                check=False,
            )
            if probe.returncode == 0:
                return
            time.sleep(0.05)
        raise SmokeFailure(f"ephemeral Postgres did not become ready; log: {self.log.read_text()}")

    def stop(self) -> None:
        if self.started:
            run([self.pg_ctl, "-D", str(self.data), "stop", "-m", "fast"], check=False)
            self.started = False

    def command(self, *extra: str) -> list[str]:
        return [
            self.psql_binary,
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            "-h",
            str(self.socket_dir),
            "-p",
            str(self.port),
            "-d",
            "postgres",
            *extra,
        ]

    def sql(self, statement: str, *, expect_failure: bool = False) -> str:
        result = run(self.command("-At", "-c", statement), check=False)
        failed = result.returncode != 0
        if failed != expect_failure:
            expectation = "failure" if expect_failure else "success"
            raise SmokeFailure(
                f"SQL expected {expectation} but exited {result.returncode}\n"
                f"statement:\n{statement}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
            )
        return result.stdout.strip()


def apply_migrations(db: EphemeralPostgres) -> None:
    for name in RELAY_MIGRATIONS:
        path = MIGRATIONS / name
        if not path.is_file():
            raise SmokeFailure(f"Relay migration is missing: {path}")
        run(db.command("-f", str(path)))


def assert_equal(actual: str, expected: str, message: str) -> None:
    if actual != expected:
        raise SmokeFailure(f"{message}: expected {expected!r}, got {actual!r}")


def verify_challenge_ownership(db: EphemeralPostgres) -> None:
    challenge = json.dumps({"nonce": "n-1", "amount_micros": 7500})
    db.sql(
        f"""
        INSERT INTO relay_aitp_challenges
          (request_id, owner_principal, challenge, state, issued_at_ms, updated_at_ms, expires_at)
        VALUES ('{REQUEST_ID}', 'alice', '{challenge}'::jsonb, 'issued', 1, 1, NOW() + INTERVAL '5 minutes')
        """
    )
    db.sql(
        f"UPDATE relay_aitp_challenges SET state='authorized' WHERE request_id='{REQUEST_ID}'",
        expect_failure=True,
    )
    db.sql(
        f"""
        UPDATE relay_aitp_challenges
        SET state='authorized', operation_hash='op-1', updated_at_ms=2
        WHERE request_id='{REQUEST_ID}' AND owner_principal='alice' AND state='issued'
        """
    )
    stolen = db.sql(
        f"""
        UPDATE relay_aitp_challenges SET state='consumed', receipt_hash='receipt-stolen'
        WHERE request_id='{REQUEST_ID}' AND owner_principal='bob'
        RETURNING request_id
        """
    )
    assert_equal(stolen, "", "another principal must not consume Alice's challenge")
    db.sql(
        f"""
        UPDATE relay_aitp_challenges
        SET state='consumed', receipt_hash='receipt-1', updated_at_ms=3
        WHERE request_id='{REQUEST_ID}' AND owner_principal='alice' AND state='authorized'
        """
    )
    state = db.sql(
        f"SELECT owner_principal || ':' || state FROM relay_aitp_challenges WHERE request_id='{REQUEST_ID}'"
    )
    assert_equal(state, "alice:consumed", "challenge lifecycle did not finish")


def insert_billing_row(db: EphemeralPostgres, request_id: str, suffix: str) -> None:
    db.sql(
        f"""
        INSERT INTO relay_billing_outbox
          (owner_principal, payer_account, payee_account, request_id,
           estimate_micros, held_amount_micros, reserve_idem_key, reserve_payload,
           hold_id, state, terminal_operation, terminal_idem_key, terminal_payload)
        VALUES
          ('alice', 'acct_alice', 'acct_treasury', '{request_id}',
           10000, 10000, 'reserve-{suffix}', '{{"amount_micros":10000}}'::jsonb,
           'hold-{suffix}', 'terminal_pending', 'capture', 'capture-{suffix}',
           '{{"amount_micros":7500}}'::jsonb)
        """
    )


def verify_atomic_settlement_and_cost_event(db: EphemeralPostgres) -> None:
    insert_billing_row(db, REQUEST_ID, "one")
    rollback = db.sql(
        f"""
        BEGIN;
        INSERT INTO relay_settlement_evidence
          (principal, request_id, receipt, attestation, verified_at_ms, settled_at_ms)
        VALUES ('alice', '{REQUEST_ID}', '{{"hash":"receipt-1"}}', '{{"key_id":"mint-1"}}', 10, 10);
        INSERT INTO relay_cost_events (event_id, request_id, owner_principal, event)
        VALUES ('{EVENT_ID}', '{REQUEST_ID}', 'alice', '{{"cost_micros":7500}}');
        SELECT 1 / 0;
        COMMIT;
        """,
        expect_failure=True,
    )
    del rollback
    empty = db.sql(
        f"""
        SELECT
          (SELECT COUNT(*) FROM relay_settlement_evidence WHERE request_id='{REQUEST_ID}') || ':' ||
          (SELECT COUNT(*) FROM relay_cost_events WHERE request_id='{REQUEST_ID}')
        """
    )
    assert_equal(empty, "0:0", "failed settlement transaction leaked partial rows")

    db.sql(
        f"""
        BEGIN;
        INSERT INTO relay_settlement_evidence
          (principal, request_id, receipt, attestation, verified_at_ms, settled_at_ms)
        VALUES ('alice', '{REQUEST_ID}', '{{"hash":"receipt-1"}}', '{{"key_id":"mint-1"}}', 10, 10);
        UPDATE relay_billing_outbox
        SET state='completed', receipt='{{"hash":"receipt-1"}}',
            attestation='{{"key_id":"mint-1"}}', completed_at=NOW(), updated_at=NOW()
        WHERE owner_principal='alice' AND request_id='{REQUEST_ID}' AND state='terminal_pending';
        INSERT INTO relay_cost_events (event_id, request_id, owner_principal, event)
        VALUES ('{EVENT_ID}', '{REQUEST_ID}', 'alice', '{{"cost_micros":7500}}');
        COMMIT;
        """
    )
    projection = db.sql(
        f"""
        SELECT b.state || ':' || (e.attestation IS NOT NULL)::text || ':' || c.attempt_count
        FROM relay_billing_outbox b
        JOIN relay_settlement_evidence e USING (request_id)
        JOIN relay_cost_events c USING (request_id)
        WHERE b.owner_principal='alice' AND b.request_id='{REQUEST_ID}'
        """
    )
    assert_equal(projection, "completed:true:0", "atomic settlement projection is inconsistent")
    db.sql(
        f"""
        INSERT INTO relay_cost_events (event_id, request_id, owner_principal, event)
        VALUES ('{SECOND_EVENT_ID}', '{REQUEST_ID}', 'alice', '{{}}')
        """,
        expect_failure=True,
    )


def verify_skip_locked_leases(db: EphemeralPostgres) -> None:
    for request_id, event_id, suffix in (
        (SECOND_REQUEST_ID, SECOND_EVENT_ID, "two"),
        (THIRD_REQUEST_ID, THIRD_EVENT_ID, "three"),
    ):
        insert_billing_row(db, request_id, suffix)
        db.sql(
            f"""
            INSERT INTO relay_cost_events (event_id, request_id, owner_principal, event)
            VALUES ('{event_id}', '{request_id}', 'alice', '{{"cost_micros":1}}')
            """
        )

    claim = """
        BEGIN;
        WITH selected AS (
          SELECT event_id FROM relay_cost_events
          WHERE published_at IS NULL AND dead_lettered_at IS NULL
            AND lease_owner IS NULL AND request_id <> '{completed}'
          ORDER BY created_at, event_id
          FOR UPDATE SKIP LOCKED LIMIT 1
        ), claimed AS (
          UPDATE relay_cost_events e
          SET lease_owner='{owner}', lease_until=NOW() + INTERVAL '30 seconds'
          FROM selected s WHERE e.event_id=s.event_id
          RETURNING e.event_id
        )
        SELECT event_id FROM claimed;
        SELECT pg_sleep(0.5);
        COMMIT;
    """
    first = subprocess.Popen(
        db.command("-At", "-c", claim.format(completed=REQUEST_ID, owner="worker-a")),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    time.sleep(0.1)
    second = run(
        db.command("-At", "-c", claim.format(completed=REQUEST_ID, owner="worker-b"))
    )
    first_stdout, first_stderr = first.communicate(timeout=5)
    if first.returncode != 0:
        raise SmokeFailure(f"first lease worker failed:\n{first_stdout}\n{first_stderr}")
    owners = db.sql(
        f"""
        SELECT string_agg(lease_owner, ',' ORDER BY lease_owner)
        FROM relay_cost_events
        WHERE request_id IN ('{SECOND_REQUEST_ID}', '{THIRD_REQUEST_ID}')
        """
    )
    assert_equal(owners, "worker-a,worker-b", "SKIP LOCKED workers claimed the same row")
    if SECOND_EVENT_ID not in first_stdout + second.stdout or THIRD_EVENT_ID not in first_stdout + second.stdout:
        raise SmokeFailure("lease workers did not return both due events")


def verify_batch_and_health_constraints(db: EphemeralPostgres) -> None:
    db.sql(
        f"UPDATE relay_billing_outbox SET state='batch_pending' WHERE request_id='{SECOND_REQUEST_ID}'",
        expect_failure=True,
    )
    db.sql(
        """
        INSERT INTO relay_provider_health (model_name, provider_name, snapshot)
        VALUES ('openai/gpt-4o', 'openai', '{"successes":1}')
        ON CONFLICT (model_name, provider_name)
        DO UPDATE SET snapshot=EXCLUDED.snapshot, updated_at=NOW()
        """
    )
    assert_equal(
        db.sql("SELECT snapshot->>'successes' FROM relay_provider_health"),
        "1",
        "provider health checkpoint did not round-trip",
    )


def main() -> int:
    try:
        with tempfile.TemporaryDirectory(prefix="relay-postgres-smoke-") as temp:
            db = EphemeralPostgres(Path(temp))
            try:
                db.start()
                apply_migrations(db)
                verify_challenge_ownership(db)
                verify_atomic_settlement_and_cost_event(db)
                verify_skip_locked_leases(db)
                verify_batch_and_health_constraints(db)
            finally:
                db.stop()
        print(
            json.dumps(
                {
                    "status": "ok",
                    "migrations": len(RELAY_MIGRATIONS),
                    "checks": [
                        "principal_bound_challenge",
                        "atomic_settlement_cost_event",
                        "idempotent_cost_event",
                        "skip_locked_leases",
                        "batch_metadata_constraint",
                        "provider_health_checkpoint",
                    ],
                },
                separators=(",", ":"),
            )
        )
        return 0
    except (SmokeFailure, subprocess.SubprocessError, OSError) as error:
        print(f"relay Postgres smoke failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
