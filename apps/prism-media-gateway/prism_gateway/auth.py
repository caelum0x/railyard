"""Fail-closed API-key authentication for the Prism public edge."""

from __future__ import annotations

import hashlib
import hmac
import os
from dataclasses import dataclass

from fastapi import HTTPException, Request


def key_fingerprint(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def configured_key_hashes() -> frozenset[str]:
    raw = os.getenv("PRISM_API_KEY_SHA256S", "")
    return frozenset(item.strip().lower() for item in raw.split(",") if item.strip())


@dataclass(frozen=True)
class Principal:
    key_fingerprint: str
    provider_key: str = ""
    provider_api_key: str = ""


class ApiKeyAuthenticator:
    def __init__(self, allowed_hashes: frozenset[str] | None = None) -> None:
        self._allowed_hashes = allowed_hashes

    def authenticate(self, request: Request) -> Principal:
        allowed = self._allowed_hashes
        if allowed is None:
            allowed = configured_key_hashes()
        if not allowed:
            raise HTTPException(
                status_code=503, detail="Prism API keys are not configured"
            )

        authorization = request.headers.get("authorization", "")
        bearer = (
            authorization[7:].strip()
            if authorization.lower().startswith("bearer ")
            else ""
        )
        raw_key = bearer or request.headers.get("x-prism-key", "").strip()
        fingerprint = key_fingerprint(raw_key) if raw_key else ""
        if not fingerprint or not any(
            hmac.compare_digest(fingerprint, expected) for expected in allowed
        ):
            raise HTTPException(status_code=401, detail="Invalid Prism API key")

        return Principal(
            key_fingerprint=fingerprint,
            provider_key=request.headers.get("x-provider", "").strip(),
            provider_api_key=request.headers.get("x-provider-api-key", "").strip(),
        )
