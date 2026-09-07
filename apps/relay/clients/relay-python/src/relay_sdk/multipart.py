from __future__ import annotations

import secrets
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class MultipartField:
    name: str
    value: str


@dataclass(frozen=True, slots=True)
class MultipartPart:
    name: str
    filename: str
    content_type: str
    body: bytes


class MultipartForm:
    """Dependency-free, binary-safe multipart/form-data builder."""

    def __init__(self) -> None:
        self._fields: list[MultipartField] = []
        self._files: list[MultipartPart] = []

    def text(self, name: str, value: str) -> MultipartForm:
        self._fields.append(MultipartField(_token(name, "multipart field name"), value))
        return self

    def file(
        self,
        name: str,
        filename: str,
        content_type: str,
        body: bytes | bytearray | memoryview,
    ) -> MultipartForm:
        if not isinstance(body, (bytes, bytearray, memoryview)):
            raise TypeError("multipart file body must be bytes-like")
        self._files.append(
            MultipartPart(
                _token(name, "multipart part name"),
                _token(filename, "multipart filename"),
                _content_type(content_type),
                bytes(body),
            )
        )
        return self

    def encode(self) -> tuple[str, bytes]:
        boundary = f"relay-sdk-{secrets.token_hex(16)}"
        body = bytearray()
        for field in self._fields:
            body.extend(
                (
                    f'--{boundary}\r\nContent-Disposition: form-data; '
                    f'name="{field.name}"\r\n\r\n'
                ).encode()
            )
            body.extend(field.value.encode())
            body.extend(b"\r\n")
        for part in self._files:
            body.extend(
                (
                    f'--{boundary}\r\nContent-Disposition: form-data; name="{part.name}"; '
                    f'filename="{part.filename}"\r\nContent-Type: {part.content_type}\r\n\r\n'
                ).encode()
            )
            body.extend(part.body)
            body.extend(b"\r\n")
        body.extend(f"--{boundary}--\r\n".encode())
        return f"multipart/form-data; boundary={boundary}", bytes(body)


def _token(value: str, label: str) -> str:
    if not isinstance(value, str) or not value or any(char in value for char in '\r\n"'):
        raise ValueError(f"{label} is empty or contains unsafe header characters")
    return value


def _content_type(value: str) -> str:
    invalid = (
        not isinstance(value, str)
        or "/" not in value
        or not value.strip()
        or "\r" in value
        or "\n" in value
    )
    if invalid:
        raise ValueError("multipart content type is invalid")
    return value
