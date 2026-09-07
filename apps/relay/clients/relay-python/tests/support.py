from __future__ import annotations

from collections.abc import AsyncIterator, Mapping
from dataclasses import dataclass

from relay_sdk import HttpResponse, HttpStreamResponse


@dataclass
class Call:
    method: str
    url: str
    headers: dict[str, str]
    body: bytes | None


class FakeTransport:
    def __init__(self) -> None:
        self.responses: list[HttpResponse] = []
        self.streams: list[HttpStreamResponse] = []
        self.calls: list[Call] = []

    def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse:
        self.calls.append(Call(method, url, dict(headers), body))
        if not self.responses:
            raise AssertionError("no fake response queued")
        return self.responses.pop(0)

    def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpStreamResponse:
        self.calls.append(Call(method, url, dict(headers), body))
        if not self.streams:
            raise AssertionError("no fake stream queued")
        return self.streams.pop(0)


class FakeAsyncTransport:
    def __init__(self) -> None:
        self.responses: list[HttpResponse] = []
        self.streams: list[tuple[int, dict[str, str], list[bytes]]] = []
        self.calls: list[Call] = []

    async def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse:
        self.calls.append(Call(method, url, dict(headers), body))
        if not self.responses:
            raise AssertionError("no fake response queued")
        return self.responses.pop(0)

    async def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> tuple[int, dict[str, str], AsyncIterator[bytes]]:
        self.calls.append(Call(method, url, dict(headers), body))
        if not self.streams:
            raise AssertionError("no fake stream queued")
        status, response_headers, chunks = self.streams.pop(0)

        async def iterator() -> AsyncIterator[bytes]:
            for chunk in chunks:
                yield chunk

        return status, response_headers, iterator()


def json_response(status: int, value: object, headers: dict[str, str] | None = None) -> HttpResponse:
    import json

    return HttpResponse(status, headers or {}, json.dumps(value).encode())
