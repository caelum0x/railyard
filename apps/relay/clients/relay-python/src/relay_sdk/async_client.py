from __future__ import annotations

import json
from collections import deque
from collections.abc import AsyncIterator, Callable, Mapping
from typing import Any
from urllib.parse import urlencode

from .aitp import (
    AITP_AUTHORIZATION_HEADER,
    AITP_REQUEST_ID_HEADER,
    payment_required_from_headers,
    validate_header,
    validate_uuid,
)
from .client import _chat, _data, _encode, _normalize_base_url, _safe_path, _token_count
from .errors import (
    RelayAlreadySettledError,
    RelayApiError,
    RelayAuthenticationError,
    RelayConfigurationError,
    RelayInvalidSettlementEvidenceError,
    RelayPaymentRequiredError,
    RelayProtocolError,
    RelayResponseTooLarge,
    RelayUnverifiedSettlementEvidenceError,
)
from .multipart import MultipartForm
from .sse import SseDecoder
from .transport import AsyncThreadTransport, AsyncTransport, HttpResponse
from .types import AsyncAitpAuthorizer, JsonObject, SettlementEvidence, settlement_evidence_from_value


class AsyncRelayStream(AsyncIterator[JsonObject]):
    def __init__(
        self,
        chunks: AsyncIterator[bytes],
        headers: Mapping[str, str],
        *,
        max_event_bytes: int,
        on_abandoned: Callable[[str], None] | None,
    ) -> None:
        self._chunks = chunks
        self._decoder = SseDecoder(max_event_bytes=max_event_bytes)
        self._pending: deque[JsonObject] = deque()
        self._request_id = headers.get(AITP_REQUEST_ID_HEADER)
        self._on_abandoned = on_abandoned
        self._closed = False

    def __aiter__(self) -> AsyncRelayStream:
        return self

    async def __anext__(self) -> JsonObject:
        try:
            while not self._pending:
                if self._decoder.done:
                    await self.aclose()
                    raise StopAsyncIteration
                try:
                    chunk = await anext(self._chunks)
                except StopAsyncIteration:
                    self._pending.extend(self._decoder.finish())
                    if not self._pending:
                        await self.aclose()
                        raise
                else:
                    self._pending.extend(self._decoder.feed(chunk))
                    if self._decoder.done and not self._pending:
                        await self.aclose()
                        raise StopAsyncIteration
            return self._pending.popleft()
        except StopAsyncIteration:
            raise
        except Exception:
            await self.aclose()
            raise

    async def aclose(self) -> None:
        if self._closed:
            return
        self._closed = True
        closer = getattr(self._chunks, "aclose", None)
        if callable(closer):
            await closer()
        self._notify_abandoned()

    async def __aenter__(self) -> AsyncRelayStream:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()

    def __del__(self) -> None:
        try:
            if not self._closed:
                self._closed = True
                self._notify_abandoned()
        except Exception:
            pass

    def _notify_abandoned(self) -> None:
        if not self._decoder.done and self._request_id and self._on_abandoned:
            self._on_abandoned(self._request_id)


class AsyncRelayClient:
    def __init__(
        self,
        base_url: str,
        *,
        api_key: str | None = None,
        transport: AsyncTransport | None = None,
        max_response_bytes: int = 16 * 1024 * 1024,
        max_sse_event_bytes: int = 2 * 1024 * 1024,
        on_abandoned_stream: Callable[[str], None] | None = None,
        settlement_evidence_verifier: Callable[[SettlementEvidence], bool] | None = None,
    ) -> None:
        self._base_url = _normalize_base_url(base_url)
        self._api_key = validate_header(api_key, "Relay API key") if api_key else None
        if max_response_bytes <= 0 or max_sse_event_bytes <= 0:
            raise RelayConfigurationError("Relay response limits must be positive")
        self._transport = transport or AsyncThreadTransport()
        self._max_response_bytes = max_response_bytes
        self._max_sse_event_bytes = max_sse_event_bytes
        self._on_abandoned_stream = on_abandoned_stream
        self._settlement_evidence_verifier = settlement_evidence_verifier

    async def status(self) -> JsonObject:
        return await self._json("GET", "status")

    async def health(self) -> JsonObject:
        return await self._json("GET", "health", accepted_status=503)

    async def catalog(self) -> JsonObject:
        return _data(await self._json("GET", "v1/relay/catalog"))

    async def quote(
        self, model: str, *, input_tokens: int, output_tokens: int
    ) -> JsonObject:
        if not model.strip():
            raise ValueError("quote model is required")
        _token_count(input_tokens, "input_tokens")
        _token_count(output_tokens, "output_tokens")
        return _data(
            await self._json(
                "POST",
                "v1/relay/quotes",
                {
                    "model": model,
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                },
            )
        )

    async def chat_completions(self, request: Mapping[str, Any]) -> JsonObject:
        return await self._json(
            "POST", "openai/v1/chat/completions", _chat(request, stream=False)
        )

    async def paid_chat_completions(
        self, request: Mapping[str, Any], authorizer: AsyncAitpAuthorizer
    ) -> JsonObject:
        response = await self._paid_response(
            "openai/v1/chat/completions", _chat(request, stream=False), authorizer
        )
        return self._decode(response)

    async def chat_completions_stream(
        self, request: Mapping[str, Any]
    ) -> AsyncRelayStream:
        return await self._stream(
            "openai/v1/chat/completions", _chat(request, stream=True)
        )

    async def paid_chat_completions_stream(
        self, request: Mapping[str, Any], authorizer: AsyncAitpAuthorizer
    ) -> AsyncRelayStream:
        return await self._paid_stream(
            "openai/v1/chat/completions", _chat(request, stream=True), authorizer
        )

    async def embeddings(self, request: Mapping[str, Any]) -> JsonObject:
        return await self._json("POST", "openai/v1/embeddings", dict(request))

    async def paid_embeddings(
        self, request: Mapping[str, Any], authorizer: AsyncAitpAuthorizer
    ) -> JsonObject:
        return self._decode(
            await self._paid_response("openai/v1/embeddings", request, authorizer)
        )

    async def inference(self, request: Mapping[str, Any]) -> JsonObject:
        return await self._json("POST", "inference", dict(request))

    async def paid_inference(
        self, request: Mapping[str, Any], authorizer: AsyncAitpAuthorizer
    ) -> JsonObject:
        return self._decode(await self._paid_response("inference", request, authorizer))

    async def batch_inference(self, request: Mapping[str, Any]) -> JsonObject:
        return await self._json("POST", "batch_inference", dict(request))

    async def paid_batch_inference(
        self, request: Mapping[str, Any], authorizer: AsyncAitpAuthorizer
    ) -> JsonObject:
        return self._decode(
            await self._paid_response("batch_inference", request, authorizer)
        )

    async def batch_inference_status(self, batch_id: str) -> JsonObject:
        validate_uuid(batch_id, "batch id")
        return await self._json("GET", f"batch_inference/{batch_id}")

    async def settlement_evidence(self, request_id: str) -> JsonObject:
        value = await self.settlement_evidence_unverified(request_id)
        evidence = settlement_evidence_from_value(value)
        if evidence is None:
            raise RelayProtocolError("Relay returned malformed settlement evidence")
        if self._settlement_evidence_verifier is None:
            raise RelayUnverifiedSettlementEvidenceError(evidence, body=value)
        try:
            verified = self._settlement_evidence_verifier(evidence)
        except Exception:
            verified = False
        if not verified:
            raise RelayInvalidSettlementEvidenceError(evidence, body=value)
        return value

    async def settlement_evidence_unverified(self, request_id: str) -> JsonObject:
        validate_uuid(request_id, "request id")
        return _data(await self._json("GET", f"v1/billing/settlements/{request_id}"))

    async def cost_events(
        self,
        *,
        state: str | None = None,
        before: str | None = None,
        limit: int = 50,
    ) -> JsonObject:
        if limit < 1 or limit > 100:
            raise ValueError("cost event limit must be between 1 and 100")
        if state not in (None, "pending", "published", "dead_lettered"):
            raise ValueError("invalid cost event state")
        if before:
            validate_uuid(before, "cost event cursor")
        query: dict[str, str | int] = {"limit": limit}
        if state:
            query["state"] = state
        if before:
            query["before"] = before
        return _data(
            await self._json("GET", f"v1/billing/cost-events?{urlencode(query)}")
        )

    async def cost_event(self, event_id: str) -> JsonObject:
        validate_uuid(event_id, "cost event id")
        return _data(await self._json("GET", f"v1/billing/cost-events/{event_id}"))

    async def replay_cost_event(self, event_id: str) -> JsonObject:
        validate_uuid(event_id, "cost event id")
        return _data(
            await self._json("POST", f"v1/billing/cost-events/{event_id}/replay", {})
        )

    async def custom_json(
        self, method: str, path: str, body: Mapping[str, Any] | None = None
    ) -> JsonObject:
        return await self._json(method.upper(), path, body)

    async def multipart(self, path: str, form: MultipartForm) -> JsonObject:
        content_type, payload = form.encode()
        response = await self._request_bytes("POST", path, payload, content_type)
        if not 200 <= response.status < 300:
            raise self._error(response.status, response.headers, response.body)
        return self._decode(response)

    async def paid_multipart(
        self, path: str, form: MultipartForm, authorizer: AsyncAitpAuthorizer
    ) -> JsonObject:
        content_type, payload = form.encode()
        first = await self._request_bytes("POST", path, payload, content_type)
        if first.status != 402:
            if not 200 <= first.status < 300:
                raise self._error(first.status, first.headers, first.body)
            return self._decode(first)
        payment_error = self._error(first.status, first.headers, first.body)
        if not isinstance(payment_error, RelayPaymentRequiredError):
            raise payment_error
        request_id = payment_error.payment.request_id
        if request_id is None:
            raise RelayProtocolError("AITP challenge is missing its request id")
        authorization = validate_header(
            await authorizer(payment_error.payment), "AITP authorization"
        )
        second = await self._request_bytes(
            "POST",
            path,
            payload,
            content_type,
            extra_headers={
                AITP_AUTHORIZATION_HEADER: authorization,
                AITP_REQUEST_ID_HEADER: request_id,
            },
        )
        if not 200 <= second.status < 300:
            raise self._error(second.status, second.headers, second.body)
        return self._decode(second)

    async def _json(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None = None,
        *,
        accepted_status: int | None = None,
    ) -> JsonObject:
        response = await self._request(method, path, body)
        if not 200 <= response.status < 300 and response.status != accepted_status:
            raise self._error(response.status, response.headers, response.body)
        return self._decode(response)

    async def _stream(self, path: str, body: Mapping[str, Any]) -> AsyncRelayStream:
        status, headers, chunks = await self._stream_request(path, body)
        if not 200 <= status < 300:
            error_body = await _collect(chunks, self._max_response_bytes)
            raise self._error(status, headers, error_body)
        return AsyncRelayStream(
            chunks,
            headers,
            max_event_bytes=self._max_sse_event_bytes,
            on_abandoned=self._on_abandoned_stream,
        )

    async def _paid_response(
        self,
        path: str,
        body: Mapping[str, Any],
        authorizer: AsyncAitpAuthorizer,
    ) -> HttpResponse:
        first = await self._request("POST", path, body)
        if first.status != 402:
            if not 200 <= first.status < 300:
                raise self._error(first.status, first.headers, first.body)
            return first
        payment_error = self._error(first.status, first.headers, first.body)
        if not isinstance(payment_error, RelayPaymentRequiredError):
            raise payment_error
        request_id = payment_error.payment.request_id
        if request_id is None:
            raise RelayProtocolError("AITP challenge is missing its request id")
        authorization = validate_header(
            await authorizer(payment_error.payment), "AITP authorization"
        )
        second = await self._request(
            "POST",
            path,
            body,
            extra_headers={
                AITP_AUTHORIZATION_HEADER: authorization,
                AITP_REQUEST_ID_HEADER: request_id,
            },
        )
        if not 200 <= second.status < 300:
            raise self._error(second.status, second.headers, second.body)
        return second

    async def _paid_stream(
        self,
        path: str,
        body: Mapping[str, Any],
        authorizer: AsyncAitpAuthorizer,
    ) -> AsyncRelayStream:
        status, headers, chunks = await self._stream_request(path, body)
        if status == 402:
            payment_error = self._error(
                status, headers, await _collect(chunks, self._max_response_bytes)
            )
            if not isinstance(payment_error, RelayPaymentRequiredError):
                raise payment_error
            request_id = payment_error.payment.request_id
            if request_id is None:
                raise RelayProtocolError("AITP challenge is missing its request id")
            authorization = validate_header(
                await authorizer(payment_error.payment), "AITP authorization"
            )
            status, headers, chunks = await self._stream_request(
                path,
                body,
                extra_headers={
                    AITP_AUTHORIZATION_HEADER: authorization,
                    AITP_REQUEST_ID_HEADER: request_id,
                },
            )
        if not 200 <= status < 300:
            raise self._error(
                status, headers, await _collect(chunks, self._max_response_bytes)
            )
        return AsyncRelayStream(
            chunks,
            headers,
            max_event_bytes=self._max_sse_event_bytes,
            on_abandoned=self._on_abandoned_stream,
        )

    async def _request(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None,
        *,
        extra_headers: Mapping[str, str] | None = None,
    ) -> HttpResponse:
        if not method.isascii() or not method.isalpha() or len(method) > 16:
            raise ValueError("Relay HTTP method is invalid")
        payload = None if method == "GET" else _encode(body or {})
        return await self._request_bytes(
            method,
            path,
            payload,
            "application/json" if payload is not None else None,
            extra_headers=extra_headers,
        )

    async def _request_bytes(
        self,
        method: str,
        path: str,
        payload: bytes | None,
        content_type: str | None,
        *,
        extra_headers: Mapping[str, str] | None = None,
    ) -> HttpResponse:
        return await self._transport.request(
            method,
            self._url(path),
            self._headers("application/json", content_type, extra_headers),
            payload,
        )

    async def _stream_request(
        self,
        path: str,
        body: Mapping[str, Any],
        *,
        extra_headers: Mapping[str, str] | None = None,
    ) -> tuple[int, dict[str, str], AsyncIterator[bytes]]:
        payload = _encode(body)
        return await self._transport.stream(
            "POST",
            self._url(path),
            self._headers("text/event-stream", "application/json", extra_headers),
            payload,
        )

    def _headers(
        self,
        accept: str,
        content_type: str | None,
        extra: Mapping[str, str] | None,
    ) -> dict[str, str]:
        headers = {"accept": accept}
        if content_type:
            headers["content-type"] = content_type
        if self._api_key:
            headers["authorization"] = f"Bearer {self._api_key}"
        for name, value in (extra or {}).items():
            headers[validate_header(name, "header name").lower()] = validate_header(
                value, f"header {name}"
            )
        return headers

    def _url(self, path: str) -> str:
        if not _safe_path(path):
            raise ValueError("Relay request path must be a safe relative path")
        return f"{self._base_url}/{path}"

    def _decode(self, response: HttpResponse) -> JsonObject:
        if len(response.body) > self._max_response_bytes:
            raise RelayResponseTooLarge(self._max_response_bytes)
        try:
            value = json.loads(response.body)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise RelayProtocolError("Relay returned malformed JSON") from error
        if not isinstance(value, dict):
            raise RelayProtocolError("Relay JSON response must be an object")
        return value

    def _error(self, status: int, headers: Mapping[str, str], body: bytes) -> RelayApiError:
        if len(body) > self._max_response_bytes:
            raise RelayResponseTooLarge(self._max_response_bytes)
        try:
            parsed: Any = json.loads(body) if body else None
        except (json.JSONDecodeError, UnicodeDecodeError):
            parsed = body.decode("utf-8", errors="replace")[:512]
        if status == 402:
            return RelayPaymentRequiredError(payment_required_from_headers(headers), body=parsed)
        error = parsed.get("error", {}) if isinstance(parsed, dict) else {}
        if not isinstance(error, dict):
            error = {}
        message = error.get("message")
        if not isinstance(message, str):
            message = f"Relay request failed with HTTP {status}"
        error_type = error.get("type") if isinstance(error.get("type"), str) else None
        code = error.get("code") if isinstance(error.get("code"), str) else None
        if status == 409 and code == "request_already_settled" and isinstance(parsed, dict):
            evidence = settlement_evidence_from_value(parsed.get("data"))
            if evidence is not None:
                if self._settlement_evidence_verifier is None:
                    return RelayUnverifiedSettlementEvidenceError(evidence, body=parsed)
                try:
                    verified = self._settlement_evidence_verifier(evidence)
                except Exception:
                    verified = False
                if verified:
                    return RelayAlreadySettledError(evidence, body=parsed)
                return RelayInvalidSettlementEvidenceError(evidence, body=parsed)
        error_class = RelayAuthenticationError if status in (401, 403) else RelayApiError
        return error_class(
            message, status=status, error_type=error_type, code=code, body=parsed
        )


async def _collect(chunks: AsyncIterator[bytes], limit: int) -> bytes:
    body = bytearray()
    async for chunk in chunks:
        body.extend(chunk)
        if len(body) > limit:
            raise RelayResponseTooLarge(limit)
    return bytes(body)
