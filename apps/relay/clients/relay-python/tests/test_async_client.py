from __future__ import annotations

import asyncio
import base64
import json

from relay_sdk import (
    AsyncRelayClient,
    MultipartForm,
    RelayAlreadySettledError,
    RelayUnverifiedSettlementEvidenceError,
)

from support import FakeAsyncTransport, json_response

BASE = "https://relay.test/root"
REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"


def payment_headers() -> dict[str, str]:
    challenge = {
        "payee": "ait1qrelay",
        "amount_micros": 50,
        "currency": "AIT",
        "resource": f"POST /inference#m@{REQUEST_ID}",
        "nonce": "nonce",
        "expiry": 1_800_000_000_000,
        "scheme": "aitp-hold",
    }
    encoded = base64.urlsafe_b64encode(json.dumps(challenge).encode()).decode().rstrip("=")
    return {"aitp-challenge": encoded, "aitp-request-id": REQUEST_ID}


def test_async_paid_request_and_stream_recovery() -> None:
    async def run() -> None:
        transport = FakeAsyncTransport()
        transport.responses.extend(
            [
                json_response(402, {}, payment_headers()),
                json_response(200, {"ok": True}),
            ]
        )
        abandoned: list[str] = []
        client = AsyncRelayClient(
            BASE, transport=transport, on_abandoned_stream=abandoned.append
        )

        async def authorize(payment):  # type: ignore[no-untyped-def]
            assert payment.request_id == REQUEST_ID
            return "async-signed"

        assert await client.paid_inference({"model_name": "m"}, authorize) == {"ok": True}
        assert transport.calls[1].headers["aitp-authorization"] == "async-signed"

        frame = b'data: {"delta":"hello"}\n\n'
        transport.streams.append((200, {"aitp-request-id": REQUEST_ID}, [frame]))
        stream = await client.chat_completions_stream(
            {
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
            }
        )
        assert (await anext(stream))["delta"] == "hello"
        await stream.aclose()
        assert abandoned == [REQUEST_ID]

    asyncio.run(run())


def test_async_done_suppresses_recovery_and_health_accepts_503() -> None:
    async def run() -> None:
        transport = FakeAsyncTransport()
        transport.responses.append(json_response(503, {"postgres": "error"}))
        client = AsyncRelayClient(BASE, transport=transport)
        assert (await client.health())["postgres"] == "error"

        abandoned: list[str] = []
        client = AsyncRelayClient(
            BASE, transport=transport, on_abandoned_stream=abandoned.append
        )
        transport.streams.append(
            (
                200,
                {"aitp-request-id": REQUEST_ID},
                [b'data: {"delta":"ok"}\n\ndata: [DONE]\n\n'],
            )
        )
        stream = await client.chat_completions_stream(
            {"model": "m", "messages": [{"role": "user", "content": "hi"}]}
        )
        assert [item async for item in stream] == [{"delta": "ok"}]
        assert abandoned == []

    asyncio.run(run())


def test_async_paid_multipart_replays_exact_bytes() -> None:
    async def run() -> None:
        transport = FakeAsyncTransport()
        transport.responses.extend(
            [
                json_response(402, {}, payment_headers()),
                json_response(200, {"uploaded": True}),
            ]
        )
        client = AsyncRelayClient(BASE, transport=transport)

        async def authorize(_payment):  # type: ignore[no-untyped-def]
            return "async-upload"

        result = await client.paid_multipart(
            "v1/files",
            MultipartForm().file(
                "file", "input.bin", "application/octet-stream", bytes([13, 10, 0, 255])
            ),
            authorize,
        )
        assert result == {"uploaded": True}
        assert transport.calls[0].body == transport.calls[1].body
        assert transport.calls[1].headers["aitp-authorization"] == "async-upload"

    asyncio.run(run())


def test_async_settled_paid_retry_returns_typed_terminal_evidence() -> None:
    async def run() -> None:
        transport = FakeAsyncTransport()
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
        client = AsyncRelayClient(
            BASE,
            transport=transport,
            settlement_evidence_verifier=lambda evidence: evidence.request_id == REQUEST_ID,
        )

        async def authorize(_payment):  # type: ignore[no-untyped-def]
            return "async-signed"

        try:
            await client.paid_inference({"model_name": "m"}, authorize)
        except RelayAlreadySettledError as error:
            assert error.evidence.request_id == REQUEST_ID
            assert error.evidence.receipt.amount_micros == 42
        else:
            raise AssertionError("expected RelayAlreadySettledError")
        assert len(transport.calls) == 2

    asyncio.run(run())


def test_async_direct_settlement_lookup_requires_verification() -> None:
    async def run() -> None:
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
        transport = FakeAsyncTransport()
        transport.responses.append(json_response(200, {"data": evidence}))
        try:
            await AsyncRelayClient(BASE, transport=transport).settlement_evidence(REQUEST_ID)
        except RelayUnverifiedSettlementEvidenceError:
            pass
        else:
            raise AssertionError("expected unverified settlement evidence")

    asyncio.run(run())
