"""Synchronous, dependency-free Vend client for services and command-line tools."""

from __future__ import annotations

import json
from dataclasses import dataclass
from email.message import Message
from http.client import HTTPResponse
from typing import Any, Iterator, Literal, Mapping
from urllib.error import HTTPError
from urllib.parse import quote, urlencode, urljoin, urlparse, urlunparse
from urllib.request import Request, urlopen

JsonObject = Mapping[str, Any]


@dataclass(frozen=True)
class SettlementHeaders:
    charge_id: str | None
    base_cost_micros: int | None
    price_micros: int | None
    model: str | None
    request_id: str | None


@dataclass(frozen=True)
class ChatResult:
    body: Any
    status: int
    settlement: SettlementHeaders


@dataclass(frozen=True)
class SSEEvent:
    event: str | None
    data: str
    id: str | None
    retry: int | None


class VendApiError(RuntimeError):
    """An HTTP or Vend envelope failure with its safe response metadata."""

    def __init__(
        self,
        status: int,
        body: Any,
        settlement: SettlementHeaders,
    ) -> None:
        error = body.get("error") if isinstance(body, dict) else None
        message = (
            error.get("message")
            if isinstance(error, dict) and isinstance(error.get("message"), str)
            else f"Vend request failed with HTTP {status}"
        )
        super().__init__(message)
        self.status = status
        self.code = error.get("code") if isinstance(error, dict) else None
        self.body = body
        self.settlement = settlement


class ChatStream(Iterator[SSEEvent]):
    """One-shot SSE iterator. Closing early disconnects so Vend can release."""

    def __init__(self, response: HTTPResponse) -> None:
        self._response = response
        self.status = response.status
        self.settlement = _settlement_headers(response.headers)
        self._buffer: list[str] = []
        self._closed = False

    def __enter__(self) -> ChatStream:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def __iter__(self) -> ChatStream:
        return self

    def __next__(self) -> SSEEvent:
        if self._closed:
            raise StopIteration
        while True:
            raw = self._response.readline()
            if raw == b"":
                if self._buffer:
                    frame = "\n".join(self._buffer)
                    self._buffer.clear()
                    self.close()
                    return _parse_event(frame)
                self.close()
                raise StopIteration
            line = raw.decode("utf-8").rstrip("\r\n")
            if line == "":
                if not self._buffer:
                    continue
                frame = "\n".join(self._buffer)
                self._buffer.clear()
                return _parse_event(frame)
            self._buffer.append(line)

    def close(self) -> None:
        if not self._closed:
            self._closed = True
            self._response.close()


class VendClient:
    """Client with deliberately separate customer-session and inference keys."""

    def __init__(
        self,
        base_url: str,
        *,
        customer_session: str | None = None,
        provisioning_key: str | None = None,
        timeout: float = 120.0,
    ) -> None:
        self.base_url = _normalize_base_url(base_url)
        self.customer_session = customer_session
        self.provisioning_key = provisioning_key
        self.timeout = timeout

    def oauth_start_url(
        self,
        provider: Literal["github", "google"],
        tenant_id: str,
    ) -> str:
        return self._url(f"/v1/auth/{provider}/start") + "?" + urlencode(
            {"tenant_id": tenant_id}
        )

    def setup(self, setup_token: str, password: str) -> dict[str, Any]:
        return self._control(
            "/v1/auth/setup",
            "POST",
            {"setup_token": setup_token, "password": password},
            authenticated=False,
        )

    def login(self, tenant_id: str, email: str, password: str) -> dict[str, Any]:
        session = self._control(
            "/v1/auth/login",
            "POST",
            {"tenant_id": tenant_id, "email": email, "password": password},
            authenticated=False,
        )
        self.customer_session = session["session_token"]
        return session

    def logout(self) -> dict[str, Any]:
        result = self._control("/v1/customer/logout", "POST")
        self.customer_session = None
        return result

    def tenant(self) -> dict[str, Any]:
        return self._control("/v1/customer/tenant", "GET")

    def list_apps(self) -> list[dict[str, Any]]:
        return self._control("/v1/customer/apps", "GET")

    def get_app(self, app_id: str) -> dict[str, Any]:
        return self._control(f"/v1/customer/apps/{_segment(app_id)}", "GET")

    def create_funding_checkout(
        self, app_id: str, provider: str, amount_micros: int
    ) -> dict[str, Any]:
        """Deposit directly into the app's Mint reserve via hosted checkout."""
        return self._control(
            f"/v1/customer/apps/{_segment(app_id)}/funding-checkout",
            "POST",
            {"provider": provider, "amount_micros": amount_micros},
        )

    def create_app(self, app: JsonObject) -> dict[str, Any]:
        return self._control("/v1/customer/apps", "POST", app)

    def set_app_enabled(self, app_id: str, enabled: bool) -> dict[str, Any]:
        return self._control(
            f"/v1/customer/apps/{_segment(app_id)}/enabled",
            "PATCH",
            {"enabled": enabled},
        )

    def list_keys(self, app_id: str) -> list[dict[str, Any]]:
        return self._control(
            f"/v1/customer/apps/{_segment(app_id)}/keys", "GET"
        )

    def issue_key(
        self,
        app_id: str,
        *,
        daily_cap_micros: int,
        environment: Literal["live", "test"] = "live",
        per_end_user_daily_cap_micros: int | None = None,
    ) -> dict[str, Any]:
        return self._control(
            f"/v1/customer/apps/{_segment(app_id)}/keys",
            "POST",
            {
                "environment": environment,
                "daily_cap_micros": daily_cap_micros,
                "per_end_user_daily_cap_micros": per_end_user_daily_cap_micros,
            },
        )

    def revoke_key(self, key_id: str) -> dict[str, Any]:
        return self._control(f"/v1/customer/keys/{_segment(key_id)}", "DELETE")

    def usage(self, app_id: str, utc_day: str | None = None) -> dict[str, Any]:
        path = f"/v1/customer/apps/{_segment(app_id)}/usage"
        if utc_day:
            path += "?" + urlencode({"utc_day": utc_day})
        return self._control(path, "GET")

    def chat_completions(
        self,
        body: JsonObject,
        *,
        idempotency_key: str,
        end_user: str | None = None,
    ) -> ChatResult:
        payload = dict(body)
        payload["stream"] = False
        response = self._chat_request(payload, idempotency_key, end_user)
        try:
            data = _decode_json(response.read())
            settlement = _settlement_headers(response.headers)
            if not 200 <= response.status < 300:
                raise VendApiError(response.status, data, settlement)
            return ChatResult(data, response.status, settlement)
        finally:
            response.close()

    def chat_completions_stream(
        self,
        body: JsonObject,
        *,
        idempotency_key: str,
        end_user: str | None = None,
    ) -> ChatStream:
        payload = dict(body)
        payload["stream"] = True
        response = self._chat_request(payload, idempotency_key, end_user)
        if not 200 <= response.status < 300:
            try:
                data = _decode_json(response.read())
                raise VendApiError(
                    response.status, data, _settlement_headers(response.headers)
                )
            finally:
                response.close()
        return ChatStream(response)

    def _control(
        self,
        path: str,
        method: str,
        body: Any = None,
        *,
        authenticated: bool = True,
    ) -> Any:
        headers = {"Accept": "application/json"}
        if authenticated:
            if not self.customer_session:
                raise ValueError("customer_session is required for this operation")
            headers["Authorization"] = f"Bearer {self.customer_session}"
        status, response_headers, raw = self._request(path, method, body, headers)
        payload = _decode_json(raw)
        if (
            not 200 <= status < 300
            or not isinstance(payload, dict)
            or payload.get("success") is not True
            or "data" not in payload
        ):
            raise VendApiError(status, payload, _settlement_headers(response_headers))
        return payload["data"]

    def _chat_request(
        self,
        body: JsonObject,
        idempotency_key: str,
        end_user: str | None,
    ) -> HTTPResponse:
        if not self.provisioning_key:
            raise ValueError("provisioning_key is required for chat completions")
        if not idempotency_key:
            raise ValueError("idempotency_key is required for chat completions")
        headers = {
            "Accept": "text/event-stream" if body.get("stream") else "application/json",
            "Authorization": f"Bearer {self.provisioning_key}",
            "Content-Type": "application/json",
            "Idempotency-Key": idempotency_key,
        }
        if end_user:
            headers["Vend-End-User"] = end_user
        request = Request(
            self._url("/v1/chat/completions"),
            data=json.dumps(body, separators=(",", ":")).encode(),
            headers=headers,
            method="POST",
        )
        try:
            return urlopen(request, timeout=self.timeout)
        except HTTPError as error:
            return error

    def _request(
        self,
        path: str,
        method: str,
        body: Any,
        headers: dict[str, str],
    ) -> tuple[int, Message, bytes]:
        encoded = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            encoded = json.dumps(body, separators=(",", ":")).encode()
        request = Request(self._url(path), data=encoded, headers=headers, method=method)
        try:
            with urlopen(request, timeout=self.timeout) as response:
                return response.status, response.headers, response.read()
        except HTTPError as error:
            try:
                return error.code, error.headers, error.read()
            finally:
                error.close()

    def _url(self, path: str) -> str:
        return urljoin(self.base_url + "/", path.lstrip("/"))


def _normalize_base_url(value: str) -> str:
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("base_url must be an absolute HTTP(S) URL")
    return urlunparse((parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", "", ""))


def _segment(value: str) -> str:
    return quote(value, safe="")


def _decode_json(raw: bytes) -> Any:
    if not raw:
        return None
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return {"raw": raw.decode("utf-8", errors="replace")}


def _integer_header(headers: Message, name: str) -> int | None:
    raw = headers.get(name)
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def _settlement_headers(headers: Message) -> SettlementHeaders:
    return SettlementHeaders(
        charge_id=headers.get("Vend-Charge-Id"),
        base_cost_micros=_integer_header(headers, "Vend-Base-Cost-Micros"),
        price_micros=_integer_header(headers, "Vend-Price-Micros"),
        model=headers.get("Vend-Model"),
        request_id=headers.get("X-Request-Id"),
    )


def _parse_event(frame: str) -> SSEEvent:
    event = None
    event_id = None
    retry = None
    data: list[str] = []
    for line in frame.splitlines():
        if not line or line.startswith(":"):
            continue
        field, separator, value = line.partition(":")
        if separator and value.startswith(" "):
            value = value[1:]
        if field == "data":
            data.append(value)
        elif field == "event":
            event = value
        elif field == "id":
            event_id = value
        elif field == "retry" and value.isdigit():
            retry = int(value)
    return SSEEvent(event=event, data="\n".join(data), id=event_id, retry=retry)
