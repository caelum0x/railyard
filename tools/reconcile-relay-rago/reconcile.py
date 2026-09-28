#!/usr/bin/env python3
"""Nightly shadow-metering reconciliation between Relay and Rago (RY-RG1).

Relay writes one row per charged inference into `relay_cost_events` (database
`relay`). The cost feed delivers each row to Rago as a `relay_llm_usage` event
whose `transaction_id` is the Relay `event_id` (database `rago`, table `events`).
This job compares the two for one UTC day, event by event:

* missing      - published by Relay, absent from Rago (a dropped event)
* dead-letter  - Relay gave up delivering it
* mismatch     - present in both, different `charged_micros`
* duplicate    - the same transaction billed in more than one Rago org
* orphan       - in Rago for that day with no Relay row
* pending      - not delivered yet (overdue after RECON_PENDING_GRACE_S)

drift_micros is the sum of the money those cases put out of line, and
ry_rago_drift_ratio = drift_micros / Relay charged micros for the day. Drift
over RECON_DRIFT_PAGE (0.005, i.e. 0.5%), any dead-letter or any duplicate pages
the railyard ntfy topic at high priority; any other finding is sent at default
priority. Every figure goes to VictoriaMetrics.

Read-only: both databases are read inside READ ONLY transactions, and Rago stays
a $0 shadow; no product's charge of record changes. Standard library only.

Usage:
  reconcile.py [--day YYYY-MM-DD] [--strict] [--no-push] [--report FILE]
Exit codes: 0 run finished (findings are paged, not exit codes), 1 page
condition with --strict, 2 the job could not read a database.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import io
import json
import os
import re
import shlex
import subprocess
import sys
import time
import urllib.request
from collections import defaultdict
from dataclasses import dataclass, field

DAY_MS = 86_400_000
METRIC_CODE_RE = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")
DETAIL_LIMIT = 50


@dataclass(frozen=True)
class Config:
    psql: list[str]
    relay_db: str
    rago_db: str
    metric_code: str
    drift_page: float
    pending_grace_s: int
    expect_feed: bool
    vm_import_url: str
    state_dir: str
    monitoring_env: str

    @staticmethod
    def from_env(env: dict[str, str]) -> "Config":
        metric = env.get("RECON_METRIC_CODE", "relay_llm_usage")
        if not METRIC_CODE_RE.match(metric):
            raise ValueError("RECON_METRIC_CODE must be a metric code like relay_llm_usage")
        dbs = {"RECON_RELAY_DB": env.get("RECON_RELAY_DB", "relay"),
               "RECON_RAGO_DB": env.get("RECON_RAGO_DB", "rago")}
        for key, value in dbs.items():
            if not re.match(r"^[a-z_][a-z0-9_]{0,62}$", value):
                raise ValueError(f"{key} must be a plain database name")
        return Config(
            psql=shlex.split(env.get("RECON_PSQL", "docker exec -i postgres-postgres-1 psql -U postgres")),
            relay_db=dbs["RECON_RELAY_DB"],
            rago_db=dbs["RECON_RAGO_DB"],
            metric_code=metric,
            drift_page=float(env.get("RECON_DRIFT_PAGE", "0.005")),
            pending_grace_s=int(env.get("RECON_PENDING_GRACE_S", "3600")),
            expect_feed=env.get("RECON_EXPECT_FEED", "0") == "1",
            vm_import_url=env.get("RECON_VM_IMPORT_URL", "http://127.0.0.1:8428/api/v1/import/prometheus"),
            state_dir=env.get("RECON_STATE_DIR", "/var/lib/railyard/reconcile-relay-rago"),
            monitoring_env=env.get("RECON_MONITORING_ENV", "/etc/products/monitoring.env"),
        )


@dataclass(frozen=True)
class RelayEvent:
    event_id: str
    key: str
    charged: int
    settled_ms: int
    state: str  # published | dead | pending
    created_s: int


@dataclass(frozen=True)
class RagoEvent:
    transaction_id: str
    org: str
    key: str
    charged: int | None
    ts_ms: int


@dataclass
class Result:
    day: str
    relay_events: int = 0
    relay_micros: int = 0
    rago_micros: int = 0
    drift_micros: int = 0
    missing: list[str] = field(default_factory=list)
    dead: list[str] = field(default_factory=list)
    mismatch: list[str] = field(default_factory=list)
    duplicate: list[str] = field(default_factory=list)
    orphan: list[str] = field(default_factory=list)
    pending_overdue: list[str] = field(default_factory=list)
    pending_fresh: int = 0
    published: int = 0
    per_key_relay: dict[str, int] = field(default_factory=lambda: defaultdict(int))
    per_key_rago: dict[str, int] = field(default_factory=lambda: defaultdict(int))
    per_org_rago: dict[str, int] = field(default_factory=lambda: defaultdict(int))

    @property
    def drift_ratio(self) -> float:
        if self.relay_micros > 0:
            return self.drift_micros / self.relay_micros
        return 1.0 if self.drift_micros > 0 else 0.0

    @property
    def feed_off(self) -> bool:
        """Relay has rows for the day but has never published or dead-lettered one."""
        return self.relay_events > 0 and self.published == 0 and not self.dead

    def page(self, cfg: Config) -> bool:
        return self.drift_ratio > cfg.drift_page or bool(self.dead) or bool(self.duplicate)

    def notice(self, cfg: Config) -> bool:
        # While the cost feed is still off (emitters not flipped), pending rows are
        # expected and only show in the report and metrics.
        overdue = bool(self.pending_overdue) and (cfg.expect_feed or not self.feed_off)
        return bool(self.missing or self.mismatch or self.orphan or overdue)


# ------------------------------------------------------------------ reading ---

def run_sql(cfg: Config, db: str, sql: str, env: dict[str, str]) -> list[list[str]]:
    """Run one read-only COPY query through psql and parse the CSV rows."""
    script = (
        "SET TIME ZONE 'UTC';\n"
        "BEGIN TRANSACTION READ ONLY;\n"
        f"COPY ({sql}) TO STDOUT WITH (FORMAT csv);\n"
        "COMMIT;\n"
    )
    cmd = cfg.psql + ["-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", db]
    proc = subprocess.run(cmd, input=script, capture_output=True, text=True, timeout=600,
                          check=False, env=env)
    if proc.returncode != 0:
        raise RuntimeError(f"psql on {db} failed: {proc.stderr.strip()[:300]}")
    return list(csv.reader(io.StringIO(proc.stdout)))


def relay_sql(start_ms: int, end_ms: int) -> str:
    return (
        "SELECT event_id::text, owner_principal, (event->>'charged_micros')::bigint, "
        "(event->>'settled_at_ms')::bigint, "
        "CASE WHEN dead_lettered_at IS NOT NULL THEN 'dead' "
        "WHEN published_at IS NOT NULL THEN 'published' ELSE 'pending' END, "
        "extract(epoch FROM created_at)::bigint "
        "FROM relay_cost_events "
        f"WHERE created_at >= to_timestamp({start_ms} / 1000.0) - interval '1 day' "
        f"AND created_at < to_timestamp({end_ms} / 1000.0) + interval '1 day' "
        f"AND (event->>'settled_at_ms')::bigint >= {start_ms} "
        f"AND (event->>'settled_at_ms')::bigint < {end_ms}"
    )


def rago_sql(start_ms: int, end_ms: int, metric_code: str) -> str:
    # The feed stamps each event with the Relay settlement time, so a one-day margin
    # on each side catches every delivered row of the day; orphans are judged on ts.
    return (
        "SELECT transaction_id, organization_id::text, coalesce(properties->>'api_key_id', ''), "
        "properties->>'charged_micros', "
        "(extract(epoch FROM timestamp) * 1000)::bigint "
        "FROM events "
        f"WHERE code = '{metric_code}' AND deleted_at IS NULL "
        f"AND timestamp >= to_timestamp({start_ms} / 1000.0) - interval '1 day' "
        f"AND timestamp < to_timestamp({end_ms} / 1000.0) + interval '1 day'"
    )


def parse_int(value: str | None) -> int | None:
    if value is None or value == "":
        return None
    try:
        return int(value)
    except ValueError:
        try:
            as_float = float(value)
        except ValueError:
            return None
        return int(as_float) if as_float.is_integer() else None


def load(cfg: Config, start_ms: int, end_ms: int,
         env: dict[str, str]) -> tuple[list[RelayEvent], list[RagoEvent]]:
    relay = [
        RelayEvent(r[0].lower(), r[1], parse_int(r[2]) or 0, parse_int(r[3]) or 0, r[4], parse_int(r[5]) or 0)
        for r in run_sql(cfg, cfg.relay_db, relay_sql(start_ms, end_ms), env)
        if len(r) == 6
    ]
    rago = [
        RagoEvent(r[0].lower(), r[1], r[2], parse_int(r[3]), parse_int(r[4]) or 0)
        for r in run_sql(cfg, cfg.rago_db, rago_sql(start_ms, end_ms, cfg.metric_code), env)
        if len(r) == 5
    ]
    return relay, rago


# ---------------------------------------------------------------- comparing ---

def reconcile(day: str, start_ms: int, end_ms: int, relay: list[RelayEvent], rago: list[RagoEvent],
              cfg: Config, now_s: int) -> Result:
    res = Result(day=day)
    by_tx: dict[str, list[RagoEvent]] = defaultdict(list)
    for ev in rago:
        by_tx[ev.transaction_id].append(ev)
    relay_ids = set()

    for ev in relay:
        relay_ids.add(ev.event_id)
        res.relay_events += 1
        res.relay_micros += ev.charged
        res.per_key_relay[ev.key] += ev.charged
        if ev.state == "published":
            res.published += 1
        if ev.state == "dead":
            res.dead.append(ev.event_id)
        hits = by_tx.get(ev.event_id, [])
        if not hits:
            if ev.state == "published":
                res.missing.append(ev.event_id)
                res.drift_micros += ev.charged
            elif ev.state == "dead":
                res.drift_micros += ev.charged
            elif now_s - ev.created_s > cfg.pending_grace_s:
                res.pending_overdue.append(ev.event_id)
                if cfg.expect_feed:
                    res.drift_micros += ev.charged
            else:
                res.pending_fresh += 1
            continue
        first = hits[0]
        charged = first.charged if first.charged is not None else 0
        res.rago_micros += charged
        res.per_key_rago[ev.key] += charged
        res.per_org_rago[first.org] += charged
        if first.charged != ev.charged:
            res.mismatch.append(ev.event_id)
            res.drift_micros += abs(ev.charged - charged)
        if len(hits) > 1:
            res.duplicate.append(ev.event_id)
            for extra in hits[1:]:
                extra_charged = extra.charged or 0
                res.rago_micros += extra_charged
                res.per_org_rago[extra.org] += extra_charged
                res.drift_micros += extra_charged

    for tx, hits in by_tx.items():
        if tx in relay_ids:
            continue
        for ev in hits:
            if start_ms <= ev.ts_ms < end_ms:
                res.orphan.append(tx)
                res.rago_micros += ev.charged or 0
                res.per_org_rago[ev.org] += ev.charged or 0
                res.drift_micros += ev.charged or 0
    return res


# ---------------------------------------------------------------- reporting ---

def prom_label(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", " ")


def metrics_text(res: Result, cfg: Config, now_s: int) -> str:
    ok = 0 if res.page(cfg) else 1
    lines = [
        f"ry_rago_drift_ratio {res.drift_ratio:.6f}",
        f"ry_rago_drift_micros {res.drift_micros}",
        f"ry_rago_relay_micros {res.relay_micros}",
        f"ry_rago_rago_micros {res.rago_micros}",
        f"ry_rago_relay_events {res.relay_events}",
        f"ry_rago_missing_events {len(res.missing)}",
        f"ry_rago_dead_letter_events {len(res.dead)}",
        f"ry_rago_amount_mismatch_events {len(res.mismatch)}",
        f"ry_rago_duplicate_events {len(res.duplicate)}",
        f"ry_rago_orphan_events {len(res.orphan)}",
        f"ry_rago_pending_overdue_events {len(res.pending_overdue)}",
        f"ry_rago_feed_off {1 if res.feed_off else 0}",
        f"ry_rago_reconcile_ok {ok}",
        f"ry_rago_reconcile_last_run_timestamp_seconds {now_s}",
    ]
    for key in sorted(set(res.per_key_relay) | set(res.per_key_rago)):
        label = prom_label(key)
        lines.append(f'ry_rago_key_relay_micros{{key="{label}"}} {res.per_key_relay.get(key, 0)}')
        lines.append(f'ry_rago_key_rago_micros{{key="{label}"}} {res.per_key_rago.get(key, 0)}')
    for org in sorted(res.per_org_rago):
        lines.append(f'ry_rago_org_rago_micros{{org="{prom_label(org)}"}} {res.per_org_rago[org]}')
    return "\n".join(lines) + "\n"


def report_dict(res: Result, cfg: Config) -> dict:
    return {
        "day": res.day,
        "verdict": "PAGE" if res.page(cfg) else ("NOTICE" if res.notice(cfg) else "OK"),
        "feed_off": res.feed_off,
        "drift_ratio": round(res.drift_ratio, 6),
        "drift_page_threshold": cfg.drift_page,
        "drift_micros": res.drift_micros,
        "relay_events": res.relay_events,
        "relay_micros": res.relay_micros,
        "rago_micros": res.rago_micros,
        "published": res.published,
        "pending_fresh": res.pending_fresh,
        "counts": {
            "missing": len(res.missing), "dead_letter": len(res.dead), "mismatch": len(res.mismatch),
            "duplicate": len(res.duplicate), "orphan": len(res.orphan),
            "pending_overdue": len(res.pending_overdue),
        },
        "examples": {
            "missing": res.missing[:DETAIL_LIMIT], "dead_letter": res.dead[:DETAIL_LIMIT],
            "mismatch": res.mismatch[:DETAIL_LIMIT], "duplicate": res.duplicate[:DETAIL_LIMIT],
            "orphan": res.orphan[:DETAIL_LIMIT], "pending_overdue": res.pending_overdue[:DETAIL_LIMIT],
        },
        "per_key": {
            k: {"relay_micros": res.per_key_relay.get(k, 0), "rago_micros": res.per_key_rago.get(k, 0)}
            for k in sorted(set(res.per_key_relay) | set(res.per_key_rago))
        },
        "per_org_rago_micros": dict(sorted(res.per_org_rago.items())),
        "charge_of_record": "unchanged (read-only; Rago is a $0 shadow)",
    }


def summary_line(rep: dict) -> str:
    c = rep["counts"]
    return (
        f"{rep['day']}: drift {rep['drift_ratio'] * 100:.3f}% ({rep['drift_micros']} of "
        f"{rep['relay_micros']} micros), {rep['relay_events']} Relay events; missing {c['missing']}, "
        f"dead-letter {c['dead_letter']}, mismatch {c['mismatch']}, duplicate {c['duplicate']}, "
        f"orphan {c['orphan']}, pending overdue {c['pending_overdue']}"
        + ("; cost feed looks OFF (events stay pending)" if rep["feed_off"] else "")
    )


def read_env_file(path: str) -> dict[str, str]:
    out: dict[str, str] = {}
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                key, value = line.split("=", 1)
                out[key.strip()] = value.strip().strip('"').strip("'")
    except OSError:
        pass
    return out


def notify(cfg: Config, title: str, priority: str, body: str) -> None:
    menv = read_env_file(cfg.monitoring_env)
    url = menv.get("NTFY_URL_RAILYARD") or menv.get("NTFY_URL")
    if not url:
        print(f"[reconcile] notify (no ntfy topic configured): {title}", file=sys.stderr)
        return
    req = urllib.request.Request(url, data=body.encode(), method="POST")
    req.add_header("Title", title)
    req.add_header("Priority", priority)
    req.add_header("Tags", "railyard,rago,reconcile")
    if menv.get("NTFY_TOKEN"):
        req.add_header("Authorization", f"Bearer {menv['NTFY_TOKEN']}")
    try:
        urllib.request.urlopen(req, timeout=10).close()
    except OSError as exc:
        print(f"[reconcile] ntfy failed: {exc}", file=sys.stderr)


def push_metrics(cfg: Config, text: str) -> bool:
    req = urllib.request.Request(cfg.vm_import_url, data=text.encode(), method="POST")
    try:
        urllib.request.urlopen(req, timeout=10).close()
        return True
    except OSError as exc:
        print(f"[reconcile] VictoriaMetrics import failed: {exc}", file=sys.stderr)
        return False


# --------------------------------------------------------------------- main ---

def day_window(day: str | None, now: dt.datetime) -> tuple[str, int, int]:
    if day is None:
        d = (now - dt.timedelta(days=1)).date()
    else:
        if not re.match(r"^\d{4}-\d{2}-\d{2}$", day):
            raise ValueError("--day must be YYYY-MM-DD")
        d = dt.date.fromisoformat(day)
    start = int(dt.datetime(d.year, d.month, d.day, tzinfo=dt.timezone.utc).timestamp() * 1000)
    return d.isoformat(), start, start + DAY_MS


def main(argv: list[str] | None = None, env: dict[str, str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Reconcile Relay cost events with Rago usage (RY-RG1)")
    parser.add_argument("--day", help="UTC day to check (default: yesterday)")
    parser.add_argument("--strict", action="store_true", help="exit 1 when the run would page")
    parser.add_argument("--no-push", action="store_true", help="skip the VictoriaMetrics import")
    parser.add_argument("--no-notify", action="store_true", help="skip ntfy")
    parser.add_argument("--report", help="report file (default: RECON_STATE_DIR/<day>.json)")
    args = parser.parse_args(argv)
    env = dict(os.environ) if env is None else env
    try:
        cfg = Config.from_env(env)
        day, start_ms, end_ms = day_window(args.day, dt.datetime.now(dt.timezone.utc))
    except ValueError as exc:
        print(f"[reconcile] {exc}", file=sys.stderr)
        return 2
    now_s = int(time.time())

    try:
        relay, rago = load(cfg, start_ms, end_ms, env)
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as exc:
        print(f"[reconcile] {exc}", file=sys.stderr)
        if not args.no_notify:
            notify(cfg, f"Relay/Rago reconciliation could not run ({day})", "high", str(exc)[:500])
        return 2

    res = reconcile(day, start_ms, end_ms, relay, rago, cfg, now_s)
    rep = report_dict(res, cfg)
    line = summary_line(rep)
    print(f"RECONCILE {rep['verdict']} {line}")

    path = args.report or os.path.join(cfg.state_dir, f"{day}.json")
    try:
        os.makedirs(os.path.dirname(path) or ".", mode=0o750, exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(rep, fh, indent=2, sort_keys=True)
    except OSError as exc:
        print(f"[reconcile] could not write {path}: {exc}", file=sys.stderr)

    if not args.no_push:
        push_metrics(cfg, metrics_text(res, cfg, now_s))
    if not args.no_notify:
        if res.page(cfg):
            notify(cfg, f"Relay/Rago drift PAGE {day}", "high", line)
        elif res.notice(cfg):
            notify(cfg, f"Relay/Rago reconciliation findings {day}", "default", line)
    return 1 if (args.strict and res.page(cfg)) else 0


if __name__ == "__main__":
    sys.exit(main())
