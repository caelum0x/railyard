"""Offline tests for reconcile.py (RY-RG1): fake psql, local ntfy and VictoriaMetrics.

    python3 -m unittest discover -s tools/reconcile-relay-rago/test -v
"""

from __future__ import annotations

import http.server
import importlib.util
import json
import os
import pathlib
import stat
import sys
import tempfile
import threading
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("reconcile", HERE.parent / "reconcile.py")
reconcile = importlib.util.module_from_spec(SPEC)
sys.modules["reconcile"] = reconcile
SPEC.loader.exec_module(reconcile)

DAY = "2026-09-28"
DAY_START_MS = 1_790_553_600_000  # 2026-09-28T00:00:00Z
NOON = DAY_START_MS + 12 * 3_600_000
OLD_CREATED_S = NOON // 1000  # far older than the pending grace at test time

FAKE_PSQL = """#!/usr/bin/env python3
import os, sys
db = sys.argv[sys.argv.index("-d") + 1]
script = sys.stdin.read()
with open(os.environ["FAKE_LOG"], "a") as fh:
    fh.write(f"--- {db}\\n{script}\\n")
if os.environ.get("FAKE_FAIL") == db:
    sys.stderr.write("psql: connection refused")
    sys.exit(2)
path = os.environ["FAKE_RELAY"] if db == "relay" else os.environ["FAKE_RAGO"]
sys.stdout.write(open(path).read())
"""


class Sink(http.server.BaseHTTPRequestHandler):
    posts: list[tuple[str, dict, str]] = []

    def do_POST(self):  # noqa: N802 (http.server API)
        body = self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
        Sink.posts.append((self.path, dict(self.headers), body))
        self.send_response(204)
        self.end_headers()

    def log_message(self, *args):
        return


def relay_row(eid, key, charged, state="published", settled=NOON, created=OLD_CREATED_S):
    return f"{eid},{key},{charged},{settled},{state},{created}\n"


def rago_row(tx, org, key, charged, ts=NOON):
    return f"{tx},{org},{key},{charged},{ts}\n"


class ReconcileTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Sink)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        Sink.posts.clear()
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        psql = self.dir / "psql"
        psql.write_text(FAKE_PSQL)
        psql.chmod(psql.stat().st_mode | stat.S_IEXEC)
        (self.dir / "monitoring.env").write_text(
            f"NTFY_URL={self.base}/box\nNTFY_URL_RAILYARD={self.base}/railyard\nNTFY_TOKEN=tok\n"
        )
        self.env = {
            "RECON_PSQL": f"{sys.executable} {psql}",
            "RECON_VM_IMPORT_URL": f"{self.base}/api/v1/import/prometheus",
            "RECON_STATE_DIR": str(self.dir / "state"),
            "RECON_MONITORING_ENV": str(self.dir / "monitoring.env"),
            "FAKE_LOG": str(self.dir / "sql.log"),
            "FAKE_RELAY": str(self.dir / "relay.csv"),
            "FAKE_RAGO": str(self.dir / "rago.csv"),
            "PATH": os.environ.get("PATH", ""),
        }

    def tearDown(self):
        self.tmp.cleanup()

    def run_day(self, relay: str, rago: str, *extra: str, env: dict | None = None) -> tuple[int, dict]:
        (self.dir / "relay.csv").write_text(relay)
        (self.dir / "rago.csv").write_text(rago)
        rc = reconcile.main(["--day", DAY, *extra], env={**self.env, **(env or {})})
        path = self.dir / "state" / f"{DAY}.json"
        return rc, (json.loads(path.read_text()) if path.exists() else {})

    def ntfy(self):
        return [(h.get("Title"), h.get("Priority"), b) for p, h, b in Sink.posts if p == "/railyard"]

    def vm(self) -> str:
        return "".join(b for p, _, b in Sink.posts if p == "/api/v1/import/prometheus")

    # ------------------------------------------------------------------ cases

    def test_clean_day_is_ok_and_pushes_zero_drift(self):
        relay = relay_row("a1", "keyA", 500_000) + relay_row("a2", "keyA", 250_000) + relay_row("b1", "keyB", 10)
        rago = rago_row("a1", "org-1", "keyA", 500_000) + rago_row("a2", "org-1", "keyA", 250_000) + \
            rago_row("b1", "org-2", "keyB", 10)
        rc, rep = self.run_day(relay, rago)
        self.assertEqual(rc, 0)
        self.assertEqual(rep["verdict"], "OK")
        self.assertEqual(rep["drift_micros"], 0)
        self.assertEqual(self.ntfy(), [])
        vm = self.vm()
        self.assertIn("ry_rago_drift_ratio 0.000000", vm)
        self.assertIn('ry_rago_key_relay_micros{key="keyA"} 750000', vm)
        self.assertIn('ry_rago_org_rago_micros{org="org-2"} 10', vm)
        self.assertIn("ry_rago_reconcile_ok 1", vm)
        self.assertEqual(rep["charge_of_record"], "unchanged (read-only; Rago is a $0 shadow)")

    def test_a_deliberately_dropped_event_is_reported(self):
        relay = relay_row("big", "keyA", 1_000_000) + relay_row("dropped", "keyA", 1_000)
        rago = rago_row("big", "org-1", "keyA", 1_000_000)
        rc, rep = self.run_day(relay, rago)
        self.assertEqual(rc, 0)
        self.assertEqual(rep["verdict"], "NOTICE")
        self.assertEqual(rep["counts"]["missing"], 1)
        self.assertEqual(rep["examples"]["missing"], ["dropped"])
        self.assertIn("ry_rago_missing_events 1", self.vm())
        (title, prio, body), = self.ntfy()
        self.assertIn("findings", title)
        self.assertEqual(prio, "default")
        self.assertIn("missing 1", body)

    def test_drift_over_half_a_percent_pages(self):
        relay = relay_row("a", "keyA", 100_000) + relay_row("b", "keyA", 1_000)
        rago = rago_row("a", "org-1", "keyA", 100_000)
        rc, rep = self.run_day(relay, rago, "--strict")
        self.assertEqual(rc, 1)
        self.assertEqual(rep["verdict"], "PAGE")
        self.assertGreater(rep["drift_ratio"], 0.005)
        (title, prio, _), = self.ntfy()
        self.assertIn("drift PAGE", title)
        self.assertEqual(prio, "high")
        self.assertIn("ry_rago_reconcile_ok 0", self.vm())

    def test_any_dead_letter_pages_even_with_tiny_drift(self):
        relay = relay_row("a", "keyA", 10_000_000) + relay_row("d", "keyA", 1, state="dead")
        rago = rago_row("a", "org-1", "keyA", 10_000_000)
        rc, rep = self.run_day(relay, rago, "--strict")
        self.assertEqual(rc, 1)
        self.assertEqual(rep["counts"]["dead_letter"], 1)
        self.assertLess(rep["drift_ratio"], 0.005)
        self.assertEqual(self.ntfy()[0][1], "high")

    def test_amount_mismatch_duplicate_and_orphan(self):
        relay = relay_row("m", "keyA", 1_000_000) + relay_row("dup", "keyB", 1_000_000)
        rago = (rago_row("m", "org-1", "keyA", 999_000) + rago_row("dup", "org-1", "keyB", 1_000_000)
                + rago_row("dup", "org-2", "keyB", 1_000_000) + rago_row("ghost", "org-3", "keyC", 5))
        rc, rep = self.run_day(relay, rago)
        self.assertEqual(rep["counts"], {"missing": 0, "dead_letter": 0, "mismatch": 1, "duplicate": 1,
                                         "orphan": 1, "pending_overdue": 0})
        self.assertEqual(rep["drift_micros"], 1_000 + 1_000_000 + 5)
        self.assertEqual(rep["verdict"], "PAGE")  # a duplicate bills twice

    def test_rago_rows_outside_the_day_are_not_orphans(self):
        relay = relay_row("a", "keyA", 100)
        rago = rago_row("a", "org-1", "keyA", 100) + rago_row("yesterday", "org-1", "keyA", 7, ts=DAY_START_MS - 1)
        _, rep = self.run_day(relay, rago)
        self.assertEqual(rep["counts"]["orphan"], 0)
        self.assertEqual(rep["verdict"], "OK")

    def test_feed_off_is_reported_but_quiet_until_expected(self):
        relay = relay_row("p1", "keyA", 100, state="pending") + relay_row("p2", "keyA", 200, state="pending")
        rc, rep = self.run_day(relay, "")
        self.assertEqual(rc, 0)
        self.assertTrue(rep["feed_off"])
        self.assertEqual(rep["verdict"], "OK")
        self.assertEqual(rep["counts"]["pending_overdue"], 2)
        self.assertEqual(self.ntfy(), [])
        self.assertIn("ry_rago_feed_off 1", self.vm())

        Sink.posts.clear()
        rc, rep = self.run_day(relay, "", "--strict", env={"RECON_EXPECT_FEED": "1"})
        self.assertEqual(rc, 1)
        self.assertEqual(rep["verdict"], "PAGE")
        self.assertEqual(rep["drift_ratio"], 1.0)

    def test_fresh_pending_rows_are_not_findings(self):
        import time
        relay = relay_row("p", "keyA", 100, state="pending", created=int(time.time()))
        relay += relay_row("a", "keyA", 100)
        rago = rago_row("a", "org-1", "keyA", 100)
        _, rep = self.run_day(relay, rago)
        self.assertEqual(rep["pending_fresh"], 1)
        self.assertEqual(rep["counts"]["pending_overdue"], 0)

    def test_queries_are_read_only_and_fixed(self):
        self.run_day(relay_row("a", "k", 1), rago_row("a", "o", "k", 1))
        log = (self.dir / "sql.log").read_text()
        self.assertEqual(log.count("BEGIN TRANSACTION READ ONLY;"), 2)
        self.assertIn("FROM relay_cost_events", log)
        self.assertIn("code = 'relay_llm_usage'", log)
        import re
        for verb in ("INSERT", "UPDATE", "DELETE", "TRUNCATE", "ALTER", "DROP", "CREATE"):
            self.assertIsNone(re.search(rf"\b{verb}\b", log.upper()), verb)

    def test_database_error_exits_2_and_pages(self):
        rc, _ = self.run_day("", "", env={"FAKE_FAIL": "rago"})
        self.assertEqual(rc, 2)
        (title, prio, body), = self.ntfy()
        self.assertIn("could not run", title)
        self.assertEqual(prio, "high")
        self.assertIn("connection refused", body)

    def test_bad_inputs_are_refused(self):
        self.assertEqual(reconcile.main(["--day", "yesterday"], env=self.env), 2)
        self.assertEqual(reconcile.main(["--day", DAY], env={**self.env, "RECON_METRIC_CODE": "x'; drop"}), 2)
        self.assertEqual(reconcile.main(["--day", DAY], env={**self.env, "RECON_RAGO_DB": "rago;x"}), 2)

    def test_ntfy_uses_the_token(self):
        self.run_day(relay_row("a", "k", 1_000_000) + relay_row("b", "k", 10), rago_row("a", "o", "k", 1_000_000))
        headers = [h for p, h, _ in Sink.posts if p == "/railyard"][0]
        self.assertEqual(headers.get("Authorization"), "Bearer tok")


if __name__ == "__main__":
    unittest.main()
