"""Offline tests for the Relay provider canary (RY-RL1). No network, no keys.

    python3 -m unittest discover -s apps/relay/tools/provider-canary/tests
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TESTS = Path(__file__).resolve().parent
TOOL = TESTS.parent / "provider_canary.py"
ROUTES = TESTS.parent / "routes.json"
sys.path.insert(0, str(TESTS))
sys.path.insert(0, str(TESTS.parent))

import fake_relay  # noqa: E402
import provider_canary  # noqa: E402

ALL = ["anthropic", "groq", "openrouter", "openai"]


class CanaryTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.state = self.dir / "state.json"
        self.calls = self.dir / "calls.jsonl"
        self.set_state()
        self.server = fake_relay.start_in_thread(ROUTES, self.state, self.calls)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        (self.dir / "key").write_text("canary-key\n")

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def set_state(self, **overrides):
        state = {"key": "canary-key", "enabled": ALL, "broken": [], "no_tools": [],
                 "no_done": [], "down": False}
        state.update(overrides)
        self.state.write_text(json.dumps(state))

    def run_canary(self, **env):
        base = {
            "PATH": os.environ["PATH"],
            "RELAY_CANARY_URL": self.url,
            "RELAY_CANARY_KEY_FILE": str(self.dir / "key"),
            "RELAY_CANARY_TIMEOUT_S": "10",
        }
        base.update(env)
        proc = subprocess.run([sys.executable, str(TOOL)], env=base, capture_output=True,
                              text=True, timeout=60)
        result = json.loads(proc.stdout.strip().splitlines()[-1])
        return proc.returncode, result, proc.stderr

    def calls_made(self):
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    # -- routes ------------------------------------------------------------------

    def test_routes_json_matches_tensorzero_toml(self):
        proc = subprocess.run([sys.executable, str(TOOL), "--check-routes"], capture_output=True,
                              text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_every_named_chat_route_is_listed_with_primary_first(self):
        routes = {r["name"]: r["legs"] for r in json.loads(ROUTES.read_text())["routes"]}
        self.assertEqual(routes["claude-sonnet"][0], {"provider": "anthropic",
                                                      "model": "claude-sonnet-4-5"})
        self.assertEqual(routes["claude-sonnet"][1]["provider"], "openrouter")
        self.assertIn("openai/gpt-oss-120b", routes)

    def test_stage_names_fit_the_metric_label_rule(self):
        for route in json.loads(ROUTES.read_text())["routes"]:
            self.assertRegex(provider_canary.stage_name(route["name"]), r"^[a-z0-9_-]{1,40}$")
            for leg in route["legs"]:
                self.assertRegex(provider_canary.stage_name("leg", leg["provider"], leg["model"]),
                                 r"^[a-z0-9_-]{1,40}$")
        long = provider_canary.stage_name("leg", "openrouter", "x" * 80)
        self.assertLessEqual(len(long), 40)

    # -- healthy -----------------------------------------------------------------

    def test_healthy_run_is_green_in_format_v1(self):
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 0, result)
        self.assertTrue(result["ok"])
        self.assertEqual(result["v"], 1)
        self.assertEqual(result["canary"], "railyard.relay-providers")
        self.assertEqual(result["product"], "railyard")
        self.assertIsNone(result["failedStage"])
        names = [s["name"] for s in result["stages"]]
        self.assertEqual(names[0], "providers")
        self.assertIn("claude-sonnet", names)
        self.assertTrue(any(n.startswith("leg-openrouter") for n in names))
        self.assertIn("routes green in 3 modes", result["detail"])
        self.assertLessEqual(len(result["detail"]), 200)

    def test_calls_are_capped_tagged_uncached_and_cover_three_modes(self):
        self.run_canary()
        calls = self.calls_made()
        sonnet = [c for c in calls if c["model"] == "tensorzero::model_name::claude-sonnet"]
        self.assertEqual(len(sonnet), 3)
        self.assertEqual(sum(1 for c in sonnet if c.get("stream")), 1)
        self.assertEqual(sum(1 for c in sonnet if c.get("tools")), 1)
        for call in calls:
            self.assertTrue(call["tensorzero::include_raw_usage"])
            self.assertEqual(call["tensorzero::cache_options"], {"enabled": "off"})
            self.assertEqual(call["tensorzero::tags"]["relay_canary"], "provider-contract")
            self.assertLessEqual(call["max_tokens"], 5 if not call.get("tools") else 48)
        # A daily run stays near the ADR's budget of about 30 calls.
        self.assertLessEqual(len(calls), 40)

    # -- the acceptance case ---------------------------------------------------------

    def test_broken_anthropic_key_turns_red_even_though_openrouter_answers(self):
        self.set_state(broken=["anthropic"])
        rc, result, log = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertFalse(result["ok"])
        self.assertEqual(result["failedStage"], "claude-sonnet")
        self.assertIn("served by openrouter, expected anthropic", result["detail"])
        self.assertNotIn("canary-key", log + json.dumps(result))

    def test_broken_openrouter_floor_is_found_by_the_leg_probe(self):
        self.set_state(broken=["openrouter"])
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertTrue(result["failedStage"].startswith("leg-openrouter"), result)

    def test_required_provider_without_key_fails_at_providers(self):
        self.set_state(enabled=["groq", "openrouter", "openai"])
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertEqual(result["failedStage"], "providers")
        self.assertIn("not configured: anthropic", result["detail"])

    def test_optional_provider_without_key_expects_the_next_leg(self):
        self.set_state(enabled=["anthropic", "groq", "openrouter"])
        rc, result, log = self.run_canary()
        self.assertEqual(rc, 0, result)
        self.assertIn("primary openai not configured, expecting openrouter", log)

    def test_route_with_no_configured_provider_is_skipped_not_red(self):
        self.set_state(enabled=["anthropic", "groq", "openrouter"])
        rc, result, _ = self.run_canary(RELAY_CANARY_REQUIRED="anthropic",
                                        RELAY_CANARY_LEGS="0")
        self.assertEqual(rc, 0, result)
        self.state.write_text(json.dumps({"key": "canary-key", "enabled": ["anthropic"],
                                          "broken": [], "no_tools": [], "no_done": []}))
        rc, result, _ = self.run_canary(RELAY_CANARY_REQUIRED="anthropic",
                                        RELAY_CANARY_LEGS="0")
        self.assertEqual(rc, 0, result)
        self.assertIn("skipped (no key)", result["detail"])

    def test_tool_contract_drift_is_red(self):
        self.set_state(no_tools=["anthropic"])
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertIn("claude-sonnet tool: no_tool_call", result["detail"])

    def test_stream_contract_drift_is_red(self):
        self.set_state(no_done=["groq"])
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertIn("stream: stream_not_done", result["detail"])

    def test_gateway_down_is_red_with_the_error_code(self):
        self.set_state(down=True)
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertIn("http_502_upstream_down", result["detail"])

    def test_wrong_key_fails_at_providers(self):
        (self.dir / "key").write_text("wrong")
        rc, result, _ = self.run_canary()
        self.assertEqual(rc, 1)
        self.assertEqual(result["failedStage"], "providers")
        self.assertIn("http_401", result["detail"])

    def test_missing_key_is_a_config_failure(self):
        rc, result, _ = self.run_canary(RELAY_CANARY_KEY_FILE=str(self.dir / "nope"))
        self.assertEqual(rc, 2)
        self.assertEqual(result["failedStage"], "config")

    def test_unreachable_gateway_is_red(self):
        rc, result, _ = self.run_canary(RELAY_CANARY_URL="http://127.0.0.1:9")
        self.assertEqual(rc, 1)
        self.assertIn("unreachable", result["detail"])

    def test_routes_json_fallback_when_toml_is_absent(self):
        rc, result, log = self.run_canary(RELAY_CANARY_CONFIG=str(self.dir / "missing.toml"))
        self.assertEqual(rc, 0, result)
        self.assertIn("using routes.json", log)


if __name__ == "__main__":
    unittest.main()
