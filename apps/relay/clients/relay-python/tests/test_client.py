from __future__ import annotations

import base64
import json

import pytest

from relay_sdk import (
    HttpStreamResponse,
    MultipartForm,
    RelayClient,
    RelayAlreadySettledError,
    RelayConfigurationError,
    RelayPaymentRequiredError,
    RelayResponseTooLarge,
    RelayUnverifiedSettlementEvidenceError,
)

from support import FakeTransport, json_response

BASE = "https://relay.test/root"
REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"


def encoded_challenge() -> str:
    challenge = {
        "payee": "ait1qrelay",
        "amount_micros": 7_500,
        "currency": "AIT",
        "resource": f"POST /openai/v1/chat/completions#m@{REQUEST_ID}",
        "nonce": "nonce",
        "expiry": 1_800_000_000_000,
        "scheme": "aitp-hold",
    }
    return base64.urlsafe_b64encode(json.dumps(challenge).encode()).decode().rstrip("=")


def payment_headers() -> dict[str, str]:
    return {
        "aitp-challenge": encoded_challenge(),
        "aitp-request-id": REQUEST_ID,
        "aitp-version": "1",
    }


def chat_request() -> dict[str, object]:
    return {
        "model": "tensorzero::model_name::openai::gpt-4o",
        "messages": [{"role": "user", "content": "hello"}],
    }


def test_configuration_and_typed_unhealthy_health() -> None:
    with pytest.raises(RelayConfigurationError):
        RelayClient("file:///tmp/relay")
    with pytest.raises(RelayConfigurationError):
        RelayClient("https://user:secret@relay.test")

    transport = FakeTransport()
    transport.responses.append(
        json_response(
            503,
            {
                "gateway": "ok",
                "clickhouse": "ok",
                "postgres": "error",
                "valkey": "ok",
                "valkey_cache": "ok",
            },
        )
    )
    health = RelayClient(BASE, transport=transport).health()
    assert health["postgres"] == "error"
    assert transport.calls[0].method == "GET"
    assert transport.calls[0].url == f"{BASE}/health"


def test_paid_chat_retries_once_with_bound_headers() -> None:
    transport = FakeTransport()
    transport.responses.extend(
        [
            json_response(402, {"error": {"message": "payment required"}}, payment_headers()),
            json_response(200, {"choices": [{"message": {"content": "ok"}}]}),
        ]
    )
    observed = []
    client = RelayClient(BASE, api_key="relay-key", transport=transport)
    result = client.paid_chat_completions(
        chat_request(), lambda payment: observed.append(payment) or "signed-authorization"
    )
    assert result["choices"][0]["message"]["content"] == "ok"
    assert observed[0].request_id == REQUEST_ID
    assert len(transport.calls) == 2
    assert transport.calls[1].headers["aitp-authorization"] == "signed-authorization"
    assert transport.calls[1].headers["aitp-request-id"] == REQUEST_ID
    assert transport.calls[1].headers["authorization"] == "Bearer relay-key"


def test_paid_multipart_replays_identical_binary_body_once() -> None:
    transport = FakeTransport()
    transport.responses.extend(
        [
            json_response(402, {}, payment_headers()),
            json_response(200, {"uploaded": True}),
        ]
    )
    form = MultipartForm().text("purpose", "batch").file(
        "file", "requests.bin", "application/octet-stream", bytes([0, 1, 255])
    )
    result = RelayClient(BASE, transport=transport).paid_multipart(
        "v1/files", form, lambda _: "signed-upload"
    )

    assert result == {"uploaded": True}
    assert len(transport.calls) == 2
    assert transport.calls[0].body == transport.calls[1].body
    assert bytes([0, 1, 255]) in (transport.calls[0].body or b"")
    content_type = transport.calls[0].headers["content-type"]
    assert content_type.startswith("multipart/form-data; boundary=relay-sdk-")
    boundary = content_type.split("boundary=", 1)[1]
    assert (transport.calls[0].body or b"").endswith(f"--{boundary}--\r\n".encode())
    assert transport.calls[1].headers["aitp-authorization"] == "signed-upload"


def test_multipart_rejects_header_injection() -> None:
    with pytest.raises(ValueError, match="unsafe header"):
        MultipartForm().file(
            "file", "bad\r\nX-Evil: yes", "application/octet-stream", b""
        )


def test_second_payment_challenge_is_not_looped() -> None:
    transport = FakeTransport()
    payment = json_response(402, {}, payment_headers())
    transport.responses.extend([payment, payment])
    client = RelayClient(BASE, transport=transport)
    with pytest.raises(RelayPaymentRequiredError):
        client.paid_inference({"model_name": "m"}, lambda _: "signed")
    assert len(transport.calls) == 2


def test_settled_evidence_without_verifier_fails_closed() -> None:
    transport = FakeTransport()
    transport.responses.append(
        json_response(
            409,
            {
                "error": {
                    "message": "request already settled",
                    "type": "request_already_settled",
                    "code": "request_already_settled",
                },
                "data": {
                    "request_id": REQUEST_ID,
                    "principal": "acct_owner",
                    "receipt": {
                        "sequence": 7,
                        "operation": "hold.capture",
                        "operation_hash": "op",
                        "amount_micros": 42,
                        "receipt_hash": "hash",
                        "signature": "sig",
                    },
                    "settled_at_ms": 123,
                },
            },
        )
    )
    with pytest.raises(RelayUnverifiedSettlementEvidenceError):
        RelayClient(BASE, transport=transport).inference({"model_name": "m"})


def test_direct_settlement_lookup_requires_verification() -> None:
    evidence = {
        "request_id": REQUEST_ID,
        "principal": "acct_owner",
        "receipt": {
            "sequence": 7,
            "operation": "hold.capture",
            "operation_hash": "op",
            "amount_micros": 42,
            "receipt_hash": "hash",
            "signature": "sig",
        },
        "settled_at_ms": 123,
    }
    unverified_transport = FakeTransport()
    unverified_transport.responses.append(json_response(200, {"data": evidence}))
    with pytest.raises(RelayUnverifiedSettlementEvidenceError):
        RelayClient(BASE, transport=unverified_transport).settlement_evidence(REQUEST_ID)

    verified_transport = FakeTransport()
    verified_transport.responses.append(json_response(200, {"data": evidence}))
    result = RelayClient(
        BASE,
        transport=verified_transport,
        settlement_evidence_verifier=lambda candidate: candidate.request_id == REQUEST_ID,
    ).settlement_evidence(REQUEST_ID)
    assert result == evidence


def test_settled_paid_retry_returns_typed_terminal_evidence() -> None:
    transport = FakeTransport()
    transport.responses.extend(
        [
            json_response(402, {}, payment_headers()),
            json_response(
                409,
                {
                    "error": {
                        "message": "request already settled",
                        "type": "request_already_settled",
                        "code": "request_already_settled",
                    },
                    "data": {
                        "request_id": REQUEST_ID,
                        "principal": "acct_owner",
                        "receipt": {
                            "sequence": 7,
                            "operation": "hold.capture",
                            "operation_hash": "op",
                            "amount_micros": 42,
                            "receipt_hash": "hash",
                            "signature": "sig",
                        },
                        "settled_at_ms": 123,
                    },
                },
            ),
        ]
    )
    client = RelayClient(
        BASE,
        transport=transport,
        settlement_evidence_verifier=lambda evidence: evidence.request_id == REQUEST_ID,
    )

    with pytest.raises(RelayAlreadySettledError) as caught:
        client.paid_inference({"model_name": "m"}, lambda _: "signed")
    assert caught.value.evidence.request_id == REQUEST_ID
    assert caught.value.evidence.receipt.amount_micros == 42
    assert len(transport.calls) == 2


def test_catalog_quote_and_cost_event_pagination_use_real_routes() -> None:
    transport = FakeTransport()
    transport.responses.extend(
        [
            json_response(200, {"data": {"schema_version": 1, "revision": "r1", "models": []}}),
            json_response(200, {"data": {"amount_micros": 7_500}}),
            json_response(200, {"data": {"items": [], "next_cursor": None}}),
        ]
    )
    client = RelayClient(BASE, transport=transport)
    assert client.catalog()["revision"] == "r1"
    assert client.quote("openai/gpt-4o", input_tokens=1_000, output_tokens=500)[
        "amount_micros"
    ] == 7_500
    assert client.cost_events(
        state="dead_lettered", before=REQUEST_ID, limit=25
    ) == {"items": [], "next_cursor": None}
    assert transport.calls[2].url == (
        f"{BASE}/v1/billing/cost-events?limit=25&state=dead_lettered&before={REQUEST_ID}"
    )
    with pytest.raises(ValueError):
        client.cost_events(limit=101)


def test_stream_drop_enqueues_recovery_but_done_suppresses_it() -> None:
    chunk = b'data: {"choices":[{"delta":{"content":"x"}}]}\n\n'
    transport = FakeTransport()
    transport.streams.extend(
        [
            HttpStreamResponse(200, {"aitp-request-id": REQUEST_ID}, iter([chunk])),
            HttpStreamResponse(
                200,
                {"aitp-request-id": REQUEST_ID},
                iter([chunk + b"data: [DONE]\n\n"]),
            ),
        ]
    )
    abandoned: list[str] = []
    client = RelayClient(
        BASE, transport=transport, on_abandoned_stream=abandoned.append
    )
    stream = client.chat_completions_stream(chat_request())
    assert next(stream)["choices"][0]["delta"]["content"] == "x"
    stream.close()
    assert abandoned == [REQUEST_ID]

    assert list(client.chat_completions_stream(chat_request()))[0]["choices"]
    assert abandoned == [REQUEST_ID]


def test_done_in_final_data_chunk_does_not_poll_transport_again() -> None:
    class OneChunk:
        def __init__(self) -> None:
            self.sent = False

        def __iter__(self):
            return self

        def __next__(self) -> bytes:
            if self.sent:
                raise AssertionError("stream was polled after DONE")
            self.sent = True
            return b'data: {"delta":"ok"}\n\ndata: [DONE]\n\n'

        def close(self) -> None:
            pass

    transport = FakeTransport()
    transport.streams.append(
        HttpStreamResponse(200, {"aitp-request-id": REQUEST_ID}, OneChunk())
    )
    stream = RelayClient(BASE, transport=transport).chat_completions_stream(chat_request())
    assert list(stream) == [{"delta": "ok"}]


def test_response_and_sse_sizes_fail_closed() -> None:
    transport = FakeTransport()
    transport.responses.append(json_response(200, {"long": "value"}))
    client = RelayClient(BASE, transport=transport, max_response_bytes=4)
    with pytest.raises(RelayResponseTooLarge):
        client.status()
