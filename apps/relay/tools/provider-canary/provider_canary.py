#!/usr/bin/env python3
"""Relay provider-contract canary (RY-RL1, docs/adr/RY-0001-relay-engine.md).

Relay keeps the frozen TensorZero fork as its engine, so provider API drift is
ours to catch. Once a day this canary calls every named chat route in
`config/tensorzero.toml` through the running gateway, the same way Rally and
Menivor call it, in three modes:

* ``plain``  one non-streaming completion,
* ``stream`` one streaming completion (SSE, must end with ``[DONE]``),
* ``tool``   one forced call of a no-argument ``ping`` tool.

Every call asks for ``tensorzero::include_raw_usage`` so the gateway reports
which provider actually served it. A route is green only when its first
configured provider served all three calls. When the primary is broken (for
example a revoked Anthropic key), TensorZero fails over to OpenRouter and the
caller still gets an answer, but the canary turns red: that is exactly the
drift the ADR wants to see before users do.

Each fallback leg (the second and later providers of a route) also gets one
plain call through the provider shorthand, so a broken paid floor is found
too. Output is canary format v1 (infra-box monitoring/canary-format.md): one
JSON object on the last stdout line. Logs go to stderr. Standard library only.

Environment:
  RELAY_CANARY_URL          gateway base URL (default http://127.0.0.1:3100)
  RELAY_CANARY_KEY_FILE     file holding a dedicated Relay API key (0600)
  RELAY_CANARY_KEY          the key itself (tests; the file wins when both set)
  RELAY_CANARY_CONFIG       tensorzero.toml to read routes from (Python 3.11+)
  RELAY_CANARY_ROUTES       routes.json fallback (default: next to this file)
  RELAY_CANARY_REQUIRED     providers that must be configured
                            (default anthropic,groq,openrouter)
  RELAY_CANARY_MAX_TOKENS   output cap for plain and stream calls (default 5)
  RELAY_CANARY_TOOL_MAX_TOKENS  output cap for the tool call (default 48)
  RELAY_CANARY_TIMEOUT_S    per-call timeout (default 60)
  RELAY_CANARY_LEGS         1 (default) also probes fallback legs, 0 skips them

Usage:
  provider_canary.py                 run the canary
  provider_canary.py --write-routes  regenerate routes.json from the toml
  provider_canary.py --check-routes  exit 1 when routes.json is stale
"""

from __future__ import annotations

import datetime as _dt
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Iterable

HERE = Path(__file__).resolve().parent
DEFAULT_CONFIG = HERE.parents[1] / "config" / "tensorzero.toml"
DEFAULT_ROUTES = HERE / "routes.json"
CANARY_ID = "railyard.relay-providers"
PRODUCT = "railyard"
MODEL_PREFIX = "tensorzero::model_name::"
PING_TOOL = {
    "type": "function",
    "function": {
        "name": "ping",
        "description": "Health check. Call it with no arguments.",
        "parameters": {"type": "object", "properties": {}},
    },
}
STAGE_RE = re.compile(r"^[a-z0-9_-]{1,40}$")


def log(message: str) -> None:
    stamp = _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    print(f"[relay-canary {stamp}] {message}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------


def routes_from_toml(text: str) -> list[dict[str, Any]]:
    """Named chat routes and their provider legs, in routing order."""
    import tomllib  # Python 3.11+

    config = tomllib.loads(text)
    routes = []
    for name, model in sorted(config.get("models", {}).items()):
        providers = model.get("providers", {})
        legs = []
        for key in model.get("routing", []):
            provider = providers.get(key, {})
            legs.append(
                {
                    "provider": provider.get("type", key),
                    "model": provider.get("model_name", name),
                }
            )
        routes.append({"name": name, "legs": legs})
    return routes


def routes_document(routes: list[dict[str, Any]]) -> str:
    doc = {
        "note": "Generated from apps/relay/config/tensorzero.toml by provider_canary.py "
        "--write-routes. Do not edit by hand; tests fail when it is stale.",
        "routes": routes,
    }
    return json.dumps(doc, indent=2) + "\n"


def load_routes(config_path: Path, routes_path: Path) -> list[dict[str, Any]]:
    try:
        return routes_from_toml(config_path.read_text())
    except ModuleNotFoundError:
        log("Python has no tomllib (needs 3.11): using routes.json")
    except FileNotFoundError:
        log(f"no tensorzero.toml at {config_path.name}: using routes.json")
    return json.loads(routes_path.read_text())["routes"]


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


class CallError(Exception):
    """A call that did not return a usable answer. ``code`` is public-safe."""

    def __init__(self, code: str, message: str = "") -> None:
        super().__init__(code)
        self.code = code
        self.message = message


class Relay:
    def __init__(self, base_url: str, key: str, timeout: float) -> None:
        self.base = base_url.rstrip("/")
        self.key = key
        self.timeout = timeout

    def _request(self, method: str, path: str, body: Any | None = None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method)
        request.add_header("Authorization", f"Bearer {self.key}")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            return urllib.request.urlopen(request, timeout=self.timeout)
        except urllib.error.HTTPError as error:
            raw = error.read().decode("utf-8", "replace")
            code = f"http_{error.code}"
            try:
                parsed = json.loads(raw)
                inner = parsed.get("error", parsed)
                if isinstance(inner, dict) and isinstance(inner.get("code"), str):
                    code = f"http_{error.code}_{inner['code']}"
                message = inner.get("message", raw) if isinstance(inner, dict) else raw
            except (ValueError, AttributeError):
                message = raw
            raise CallError(code, str(message)[:300]) from None
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise CallError("unreachable", str(error)[:300]) from None

    def get_json(self, path: str) -> Any:
        with self._request("GET", path) as response:
            return json.loads(response.read())

    def post_json(self, path: str, body: Any) -> Any:
        with self._request("POST", path, body) as response:
            try:
                return json.loads(response.read())
            except ValueError:
                raise CallError("bad_json", "response was not JSON") from None

    def post_stream(self, path: str, body: Any) -> list[Any]:
        chunks: list[Any] = []
        done = False
        with self._request("POST", path, body) as response:
            for raw in response:
                line = raw.decode("utf-8", "replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    done = True
                    break
                try:
                    chunk = json.loads(payload)
                except ValueError:
                    raise CallError("bad_chunk", payload[:200]) from None
                if isinstance(chunk, dict) and chunk.get("error"):
                    err = chunk["error"]
                    code = err.get("code") if isinstance(err, dict) else None
                    raise CallError(f"stream_error_{code or 'unknown'}", json.dumps(err)[:300])
                chunks.append(chunk)
        if not done:
            raise CallError("stream_not_done", "stream ended without [DONE]")
        if not chunks:
            raise CallError("stream_empty", "no chunks before [DONE]")
        return chunks


# ---------------------------------------------------------------------------
# Probes
# ---------------------------------------------------------------------------


def served_providers(entries: Iterable[Any]) -> list[str]:
    out = []
    for entry in entries or []:
        if isinstance(entry, dict) and isinstance(entry.get("provider_type"), str):
            out.append(entry["provider_type"])
    return out


def request_body(model: str, mode: str, max_tokens: int, tool_max_tokens: int) -> dict:
    body: dict[str, Any] = {
        "model": MODEL_PREFIX + model,
        "messages": [{"role": "user", "content": "Reply with the single word: ok"}],
        "max_tokens": max_tokens,
        "tensorzero::include_raw_usage": True,
        "tensorzero::cache_options": {"enabled": "off"},
        "tensorzero::tags": {"relay_canary": "provider-contract", "relay_canary_mode": mode},
    }
    if mode == "stream":
        body["stream"] = True
        body["stream_options"] = {"include_usage": True}
    if mode == "tool":
        body["messages"] = [{"role": "user", "content": "Call the ping tool."}]
        body["tools"] = [PING_TOOL]
        body["tool_choice"] = {"type": "function", "function": {"name": "ping"}}
        body["max_tokens"] = tool_max_tokens
    return body


def probe(relay: Relay, model: str, mode: str, caps: tuple[int, int]) -> tuple[list[str], int]:
    """Run one call. Returns (serving providers, ms). Raises CallError."""
    body = request_body(model, mode, *caps)
    started = time.monotonic()
    path = "/openai/v1/chat/completions"
    if mode == "stream":
        chunks = relay.post_stream(path, body)
        ms = int((time.monotonic() - started) * 1000)
        if not any(isinstance(c, dict) and c.get("choices") for c in chunks):
            raise CallError("stream_no_choices", "no chunk carried choices")
        served = [p for c in chunks for p in served_providers(c.get("tensorzero_raw_usage"))]
        return served, ms
    response = relay.post_json(path, body)
    ms = int((time.monotonic() - started) * 1000)
    choices = response.get("choices") if isinstance(response, dict) else None
    if not choices or not isinstance(choices[0], dict) or "message" not in choices[0]:
        raise CallError("bad_shape", "no choices[0].message")
    usage = response.get("usage") or {}
    if not isinstance(usage.get("completion_tokens", 0), int):
        raise CallError("bad_usage", "usage.completion_tokens is not an integer")
    if mode == "tool":
        calls = choices[0]["message"].get("tool_calls") or []
        names = [c.get("function", {}).get("name") for c in calls if isinstance(c, dict)]
        if "ping" not in names:
            raise CallError("no_tool_call", f"finish_reason={choices[0].get('finish_reason')}")
    return served_providers(response.get("tensorzero_raw_usage")), ms


def stage_name(*parts: str) -> str:
    name = re.sub(r"[^a-z0-9_-]+", "-", "-".join(parts).lower()).strip("-")
    name = re.sub(r"-{2,}", "-", name) or "route"
    if len(name) > 40:
        digest = hashlib.sha1(name.encode()).hexdigest()[:6]
        name = name[:33].rstrip("-") + "-" + digest
    assert STAGE_RE.match(name), name
    return name


def env_int(name: str, default: int) -> int:
    try:
        return max(1, int(os.environ.get(name, default)))
    except ValueError:
        return default


def result_line(ok: bool, started: float, started_iso: str, stages: list, failed: str | None,
                detail: str, run: str) -> str:
    detail = re.sub(r"\s+", " ", detail).strip()[:200]
    return json.dumps(
        {
            "v": 1,
            "canary": CANARY_ID,
            "product": PRODUCT,
            "ok": ok,
            "startedAt": started_iso,
            "durationMs": int((time.monotonic() - started) * 1000),
            "stages": stages,
            "failedStage": failed,
            "detail": detail,
            "run": run,
        },
        separators=(",", ":"),
    )


def read_key() -> str:
    path = os.environ.get("RELAY_CANARY_KEY_FILE", "")
    if path:
        try:
            return Path(path).read_text().strip()
        except OSError:
            return ""
    return os.environ.get("RELAY_CANARY_KEY", "").strip()


def run() -> int:
    run_id = str(uuid.uuid4())
    started = time.monotonic()
    started_iso = _dt.datetime.now(_dt.timezone.utc).isoformat(timespec="milliseconds").replace(
        "+00:00", "Z"
    )
    stages: list[dict[str, Any]] = []
    failures: list[tuple[str, str]] = []  # (stage, public reason)
    skipped: list[str] = []

    def finish(ok: bool, detail: str, code: int) -> int:
        failed = failures[0][0] if failures else (None if ok else "config")
        print(result_line(ok, started, started_iso, stages, failed, detail, run_id), flush=True)
        return code

    key = read_key()
    if not key:
        failures.append(("config", "no Relay canary key"))
        return finish(False, "no Relay canary key (RELAY_CANARY_KEY_FILE)", 2)
    config = Path(os.environ.get("RELAY_CANARY_CONFIG") or DEFAULT_CONFIG)
    routes_path = Path(os.environ.get("RELAY_CANARY_ROUTES") or DEFAULT_ROUTES)
    try:
        routes = load_routes(config, routes_path)
    except (OSError, ValueError, KeyError) as error:
        log(f"routes: {error}")
        failures.append(("config", "no route list"))
        return finish(False, "no route list (tensorzero.toml or routes.json)", 2)

    relay = Relay(
        os.environ.get("RELAY_CANARY_URL", "http://127.0.0.1:3100"),
        key,
        float(os.environ.get("RELAY_CANARY_TIMEOUT_S", "60")),
    )
    caps = (env_int("RELAY_CANARY_MAX_TOKENS", 5), env_int("RELAY_CANARY_TOOL_MAX_TOKENS", 48))
    required = {
        p.strip()
        for p in os.environ.get("RELAY_CANARY_REQUIRED", "anthropic,groq,openrouter").split(",")
        if p.strip()
    }

    # Stage 1: which providers this Relay has keys for.
    t0 = time.monotonic()
    try:
        listing = relay.get_json("/v1/relay/provider_keys")
        slots = listing.get("data", listing).get("keys", [])
        enabled = {s["provider"] for s in slots if s.get("enabled")}
        managed = {s["provider"] for s in slots}
    except (CallError, ValueError, AttributeError, KeyError, TypeError) as error:
        code = error.code if isinstance(error, CallError) else "bad_listing"
        log(f"providers: {code} {getattr(error, 'message', '')}")
        stages.append({"name": "providers", "ms": int((time.monotonic() - t0) * 1000)})
        failures.append(("providers", f"provider list unavailable ({code})"))
        return finish(False, f"Relay provider list unavailable ({code})", 1)
    stages.append({"name": "providers", "ms": int((time.monotonic() - t0) * 1000)})
    missing_required = sorted(required - enabled)
    if missing_required:
        failures.append(("providers", f"not configured: {', '.join(missing_required)}"))

    def is_enabled(provider: str) -> bool:
        return provider in enabled or provider not in managed

    green_routes = 0
    # Stage per named route: plain, stream and tool through the route name.
    for route in routes:
        name = route["name"]
        stage = stage_name(name)
        legs = route.get("legs", [])
        live = [leg for leg in legs if is_enabled(leg["provider"])]
        if not live:
            skipped.append(name)
            log(f"{name}: skipped, no configured provider ({', '.join(l['provider'] for l in legs)})")
            continue
        expected = live[0]["provider"]
        if legs and legs[0]["provider"] != expected:
            log(f"{name}: primary {legs[0]['provider']} not configured, expecting {expected}")
        total_ms = 0
        route_ok = True
        for mode in ("plain", "stream", "tool"):
            try:
                served, ms = probe(relay, name, mode, caps)
            except CallError as error:
                log(f"{name} {mode}: {error.code} {error.message}")
                failures.append((stage, f"{name} {mode}: {error.code}"))
                route_ok = False
                break
            total_ms += ms
            log(f"{name} {mode}: {ms} ms, served by {','.join(served) or '?'}")
            if expected not in served:
                got = ",".join(sorted(set(served))) or "unknown"
                failures.append((stage, f"{name} {mode}: served by {got}, expected {expected}"))
                route_ok = False
                break
        stages.append({"name": stage, "ms": total_ms})
        green_routes += route_ok

    # Fallback legs: one plain call per distinct (provider, model) behind a primary.
    leg_total = leg_green = 0
    if os.environ.get("RELAY_CANARY_LEGS", "1") != "0":
        seen: set[tuple[str, str]] = set()
        for route in routes:
            for leg in route.get("legs", [])[1:]:
                pair = (leg["provider"], leg["model"])
                if pair in seen:
                    continue
                seen.add(pair)
                if not is_enabled(leg["provider"]):
                    continue
                leg_total += 1
                stage = stage_name("leg", leg["provider"], leg["model"])
                shorthand = f"{leg['provider']}::{leg['model']}"
                try:
                    served, ms = probe(relay, shorthand, "plain", caps)
                except CallError as error:
                    log(f"leg {shorthand}: {error.code} {error.message}")
                    stages.append({"name": stage, "ms": 0})
                    failures.append((stage, f"leg {shorthand}: {error.code}"))
                    continue
                stages.append({"name": stage, "ms": ms})
                log(f"leg {shorthand}: {ms} ms, served by {','.join(served) or '?'}")
                if leg["provider"] in served:
                    leg_green += 1
                else:
                    got = ",".join(sorted(set(served))) or "unknown"
                    failures.append((stage, f"leg {shorthand}: served by {got}"))

    probed = len(routes) - len(skipped)
    summary = f"{green_routes}/{probed} routes green in 3 modes"
    if leg_total:
        summary += f", {leg_green}/{leg_total} fallback legs"
    if skipped:
        summary += f", {len(skipped)} skipped (no key)"
    if failures:
        first = failures[0][1]
        more = f" (+{len(failures) - 1} more)" if len(failures) > 1 else ""
        return finish(False, f"{first}{more}; {summary}", 1)
    return finish(True, summary, 0)


def main(argv: list[str]) -> int:
    if argv[1:] and argv[1] in ("--write-routes", "--check-routes"):
        config = Path(os.environ.get("RELAY_CANARY_CONFIG") or DEFAULT_CONFIG)
        routes_path = Path(os.environ.get("RELAY_CANARY_ROUTES") or DEFAULT_ROUTES)
        wanted = routes_document(routes_from_toml(config.read_text()))
        if argv[1] == "--write-routes":
            routes_path.write_text(wanted)
            print(f"wrote {routes_path}")
            return 0
        current = routes_path.read_text() if routes_path.exists() else ""
        if current != wanted:
            print("routes.json is stale: run provider_canary.py --write-routes", file=sys.stderr)
            return 1
        return 0
    if argv[1:]:
        print(__doc__, file=sys.stderr)
        return 2
    return run()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
