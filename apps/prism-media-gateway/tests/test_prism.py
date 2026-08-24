from __future__ import annotations

from fastapi.testclient import TestClient

from prism_gateway.app import create_app
from prism_gateway.auth import ApiKeyAuthenticator, key_fingerprint
from prism_gateway.service import ExecutionOutcome, PrismService


class FakeExecutor:
    def __init__(self, failed_providers: set[str] | None = None) -> None:
        self.failed_providers = failed_providers or set()
        self.calls = []

    def execute(self, candidate, request, principal, correlation_id):
        self.calls.append(candidate)
        if candidate.provider_key in self.failed_providers:
            raise RuntimeError("forced outage")
        return ExecutionOutcome("completed", "https://cdn.example/result", 12_345)


def _client(executor: FakeExecutor, *keys: str) -> TestClient:
    hashes = frozenset(key_fingerprint(key) for key in keys)
    return TestClient(
        create_app(PrismService(executor=executor), ApiKeyAuthenticator(hashes))
    )


def test_catalog_exposes_image_and_video_across_at_least_three_providers():
    client = _client(FakeExecutor(), "prism-test")
    response = client.get("/v1/models")
    assert response.status_code == 200
    rows = response.json()["data"]
    assert {row["modality"] for row in rows} == {"image", "video"}
    assert len({row["provider"] for row in rows}) >= 3


def test_generation_fails_over_and_exact_cost_is_owner_scoped():
    executor = FakeExecutor({"runway"})
    client = _client(executor, "owner-key", "other-key")
    response = client.post(
        "/v1/media/generations",
        headers={"Authorization": "Bearer owner-key"},
        json={
            "model": "runwayml/gen-4",
            "prompt": "a slow camera orbit",
            "duration_seconds": 5,
            "routing": {"fallback": "equivalent", "sort": "price"},
        },
    )
    assert response.status_code == 200
    body = response.json()
    assert body["provider"] != "runway"
    assert body["cost_micros"] == 12_345
    assert [attempt["outcome"] for attempt in body["attempts"]] == ["failed", "completed"]

    found = client.get(
        f"/v1/generation?id={body['id']}", headers={"x-prism-key": "owner-key"}
    )
    assert found.status_code == 200
    hidden = client.get(
        f"/v1/generation?id={body['id']}", headers={"x-prism-key": "other-key"}
    )
    assert hidden.status_code == 404


def test_generation_auth_is_fail_closed():
    app = create_app(PrismService(executor=FakeExecutor()), ApiKeyAuthenticator(frozenset()))
    response = TestClient(app).post(
        "/v1/media/generations",
        json={"model": "runwayml/gen-4", "prompt": "x"},
    )
    assert response.status_code == 503
