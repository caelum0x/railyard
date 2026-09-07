from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator, Iterator, Mapping
from dataclasses import dataclass
from typing import Protocol
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener

from .errors import RelayTransportError


@dataclass(slots=True)
class HttpResponse:
    status: int
    headers: dict[str, str]
    body: bytes


@dataclass(slots=True)
class HttpStreamResponse:
    status: int
    headers: dict[str, str]
    chunks: Iterator[bytes]


class Transport(Protocol):
    def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse: ...

    def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpStreamResponse: ...


class AsyncTransport(Protocol):
    async def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse: ...

    async def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> tuple[int, dict[str, str], AsyncIterator[bytes]]: ...


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


class UrllibTransport:
    def __init__(self, *, timeout: float = 120.0, chunk_size: int = 64 * 1024) -> None:
        if timeout <= 0 or chunk_size <= 0:
            raise ValueError("transport timeout and chunk size must be positive")
        self._timeout = timeout
        self._chunk_size = chunk_size
        self._opener = build_opener(_NoRedirect())

    def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse:
        request = Request(url, data=body, headers=dict(headers), method=method)
        try:
            with self._opener.open(request, timeout=self._timeout) as response:
                return HttpResponse(
                    status=response.status,
                    headers=_headers(response.headers.items()),
                    body=response.read(),
                )
        except HTTPError as error:
            with error:
                return HttpResponse(
                    status=error.code,
                    headers=_headers(error.headers.items()),
                    body=error.read(),
                )
        except (URLError, TimeoutError, OSError) as error:
            raise RelayTransportError(f"Relay transport failed: {error}") from error

    def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpStreamResponse:
        request = Request(url, data=body, headers=dict(headers), method=method)
        try:
            response = self._opener.open(request, timeout=self._timeout)
        except HTTPError as error:
            return HttpStreamResponse(
                status=error.code,
                headers=_headers(error.headers.items()),
                chunks=_error_chunks(error),
            )
        except (URLError, TimeoutError, OSError) as error:
            raise RelayTransportError(f"Relay transport failed: {error}") from error

        def chunks() -> Iterator[bytes]:
            try:
                while chunk := response.read(self._chunk_size):
                    yield chunk
            finally:
                response.close()

        return HttpStreamResponse(
            status=response.status,
            headers=_headers(response.headers.items()),
            chunks=chunks(),
        )


class AsyncThreadTransport:
    """Async adapter for the dependency-free urllib transport."""

    def __init__(self, transport: Transport | None = None) -> None:
        self._transport = transport or UrllibTransport()

    async def request(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> HttpResponse:
        return await asyncio.to_thread(self._transport.request, method, url, headers, body)

    async def stream(
        self,
        method: str,
        url: str,
        headers: Mapping[str, str],
        body: bytes | None,
    ) -> tuple[int, dict[str, str], AsyncIterator[bytes]]:
        response = await asyncio.to_thread(self._transport.stream, method, url, headers, body)

        async def chunks() -> AsyncIterator[bytes]:
            while True:
                item = await asyncio.to_thread(_next_chunk, response.chunks)
                if item is None:
                    return
                yield item

        return response.status, response.headers, chunks()


def _next_chunk(chunks: Iterator[bytes]) -> bytes | None:
    try:
        return next(chunks)
    except StopIteration:
        return None


def _headers(items) -> dict[str, str]:  # type: ignore[no-untyped-def]
    return {str(name).lower(): str(value) for name, value in items}


def _error_chunks(error: HTTPError) -> Iterator[bytes]:
    try:
        yield error.read()
    finally:
        error.close()
