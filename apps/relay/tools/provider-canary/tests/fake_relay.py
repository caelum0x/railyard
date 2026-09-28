#!/usr/bin/env python3
"""A small stand-in for the Relay gateway, for the provider canary tests.

It mirrors the parts of Relay the canary depends on:

* ``GET /v1/relay/provider_keys`` lists each provider slot and whether it has a key;
* ``POST /openai/v1/chat/completions`` resolves a named route (or a
  ``provider::model`` shorthand) to its legs and fails over past providers that
  are disabled or broken, exactly like TensorZero routing, reporting the
  serving provider in ``tensorzero_raw_usage``.

State is a JSON file re-read on every request, so a test can break a provider
between runs:
  {"key": "k", "enabled": ["anthropic", ...], "broken": ["anthropic"],
   "no_tools": [], "no_done": [], "down": false}

Run: fake_relay.py <routes.json> <state.json> <calls.jsonl>; prints the port.
"""

from __future__ import annotations

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SLOTS = ("openai", "groq", "openrouter", "anthropic")


def make_server(routes_path: Path, state_path: Path, calls_path: Path) -> ThreadingHTTPServer:
    routes = {r["name"]: r["legs"] for r in json.loads(routes_path.read_text())["routes"]}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):  # quiet
            pass

        def _state(self) -> dict:
            return json.loads(state_path.read_text())

        def _send(self, code: int, body: dict) -> None:
            raw = json.dumps(body).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def _authorized(self, state: dict) -> bool:
            if self.headers.get("Authorization") != f"Bearer {state['key']}":
                self._send(401, {"error": {"code": "unauthorized", "message": "bad key"}})
                return False
            return True

        def do_GET(self):  # noqa: N802
            state = self._state()
            if not self._authorized(state):
                return
            if self.path != "/v1/relay/provider_keys":
                return self._send(404, {"error": "not found"})
            keys = [{"provider": p, "enabled": p in state["enabled"]} for p in SLOTS]
            self._send(200, {"data": {"operator": False, "keys": keys}})

        def do_POST(self):  # noqa: N802
            state = self._state()
            if not self._authorized(state):
                return
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with calls_path.open("a") as log:
                log.write(json.dumps(body) + "\n")
            if state.get("down"):
                return self._send(502, {"error": {"code": "upstream_down", "message": "x"}})
            target = body["model"].removeprefix("tensorzero::model_name::")
            if target in routes:
                legs = routes[target]
            else:
                provider, _, model = target.partition("::")
                legs = [{"provider": provider, "model": model}]
                twin = routes.get(model)
                if twin and twin[0]["provider"] == provider:
                    legs = twin  # Relay's failover twin for shorthands
            enabled = set(state["enabled"])
            if not any(leg["provider"] in enabled for leg in legs):
                return self._send(
                    503, {"error": {"code": "provider_not_configured", "message": "no key"}}
                )
            served = next(
                (leg["provider"] for leg in legs
                 if leg["provider"] in enabled and leg["provider"] not in state["broken"]),
                None,
            )
            if served is None:
                return self._send(502, {"error": {"code": "all_variants_failed", "message": "401"}})
            usage_entries = [{"provider_type": served, "api_type": "chat_completions", "data": {}}]
            if body.get("stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                chunks = [
                    {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"}}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 9, "completion_tokens": 1},
                     "tensorzero_raw_usage": usage_entries},
                ]
                for chunk in chunks:
                    self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                if served not in state.get("no_done", []):
                    self.wfile.write(b"data: [DONE]\n\n")
                return
            message = {"role": "assistant", "content": "ok"}
            if body.get("tools") and served not in state.get("no_tools", []):
                message = {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [{"id": "c1", "type": "function",
                                    "function": {"name": "ping", "arguments": "{}"}}],
                }
            self._send(
                200,
                {
                    "id": "i1",
                    "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 9, "completion_tokens": 1},
                    "tensorzero_raw_usage": usage_entries,
                },
            )

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)


def start_in_thread(routes: Path, state: Path, calls: Path) -> ThreadingHTTPServer:
    server = make_server(routes, state, calls)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


if __name__ == "__main__":
    srv = make_server(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))
    print(srv.server_address[1], flush=True)
    srv.serve_forever()
