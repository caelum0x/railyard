from __future__ import annotations

import json
from typing import Any

from .errors import RelayProtocolError


class SseDecoder:
    def __init__(self, *, max_event_bytes: int = 2 * 1024 * 1024) -> None:
        if max_event_bytes <= 0:
            raise ValueError("SSE event limit must be positive")
        self._max_event_bytes = max_event_bytes
        self._buffer = bytearray()
        self.done = False

    def feed(self, chunk: bytes) -> list[dict[str, Any]]:
        if self.done:
            return []
        self._buffer.extend(chunk)
        output: list[dict[str, Any]] = []
        while (separator := _separator(self._buffer)) is not None:
            index, length = separator
            frame = bytes(self._buffer[:index])
            del self._buffer[: index + length]
            output.extend(self._decode_frame(frame))
            if self.done:
                self._buffer.clear()
                break
        if len(self._buffer) > self._max_event_bytes:
            raise RelayProtocolError(
                f"Relay SSE event exceeded {self._max_event_bytes} bytes"
            )
        return output

    def finish(self) -> list[dict[str, Any]]:
        if self.done or not self._buffer:
            self._buffer.clear()
            return []
        frame = bytes(self._buffer)
        self._buffer.clear()
        return self._decode_frame(frame)

    def _decode_frame(self, frame: bytes) -> list[dict[str, Any]]:
        if len(frame) > self._max_event_bytes:
            raise RelayProtocolError(
                f"Relay SSE event exceeded {self._max_event_bytes} bytes"
            )
        try:
            lines = frame.decode("utf-8").replace("\r\n", "\n").split("\n")
        except UnicodeDecodeError as error:
            raise RelayProtocolError("Relay SSE frame is not valid UTF-8") from error
        data = "\n".join(
            line[5:].removeprefix(" ") for line in lines if line.startswith("data:")
        )
        if not data:
            return []
        if data == "[DONE]":
            self.done = True
            return []
        try:
            value = json.loads(data)
        except json.JSONDecodeError as error:
            raise RelayProtocolError("Relay SSE data is not valid JSON") from error
        if not isinstance(value, dict):
            raise RelayProtocolError("Relay SSE data must be a JSON object")
        return [value]


def _separator(buffer: bytearray) -> tuple[int, int] | None:
    lf = buffer.find(b"\n\n")
    crlf = buffer.find(b"\r\n\r\n")
    candidates = [(lf, 2), (crlf, 4)]
    valid = [candidate for candidate in candidates if candidate[0] >= 0]
    return min(valid, default=None, key=lambda item: item[0])
