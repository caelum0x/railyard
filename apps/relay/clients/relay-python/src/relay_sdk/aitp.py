from __future__ import annotations

import base64
import binascii
import json
from collections.abc import Mapping
from typing import Any
from uuid import UUID

from .errors import RelayProtocolError
from .types import AitpChallenge, PaymentRequired

AITP_CHALLENGE_HEADER = "aitp-challenge"
AITP_AUTHORIZATION_HEADER = "aitp-authorization"
AITP_REQUEST_ID_HEADER = "aitp-request-id"
AITP_VERSION_HEADER = "aitp-version"
AITP_RECEIPT_HEADER = "aitp-receipt"


def decode_aitp_challenge(encoded: str) -> AitpChallenge:
    try:
        padding = "=" * (-len(encoded) % 4)
        raw = base64.b64decode(encoded + padding, altchars=b"-_", validate=True)
        value = json.loads(raw)
    except (binascii.Error, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RelayProtocolError("Relay returned an invalid AITP challenge encoding") from error
    if not isinstance(value, dict):
        raise RelayProtocolError("Relay returned invalid AITP challenge fields")
    return _validate_challenge(value)


def payment_required_from_headers(headers: Mapping[str, str]) -> PaymentRequired:
    normalized = {name.lower(): value for name, value in headers.items()}
    encoded = normalized.get(AITP_CHALLENGE_HEADER)
    if not encoded:
        raise RelayProtocolError("Relay returned HTTP 402 without an AITP challenge header")
    request_id = normalized.get(AITP_REQUEST_ID_HEADER)
    if request_id is not None:
        validate_uuid(request_id, "AITP request id")
    return PaymentRequired(
        challenge=decode_aitp_challenge(encoded),
        request_id=request_id,
        protocol_version=normalized.get(AITP_VERSION_HEADER),
    )


def validate_uuid(value: str, name: str) -> None:
    try:
        parsed = UUID(value)
    except (ValueError, AttributeError) as error:
        raise ValueError(f"{name} must be a UUID") from error
    if str(parsed) != value.lower():
        raise ValueError(f"{name} must be a canonical UUID")


def validate_header(value: str, name: str) -> str:
    if not isinstance(value, str) or not value.strip() or "\r" in value or "\n" in value:
        raise ValueError(f"{name} must not be empty or contain newlines")
    return value


def _validate_challenge(value: dict[str, Any]) -> AitpChallenge:
    required_strings = ("payee", "resource", "nonce")
    if any(not isinstance(value.get(key), str) or not value[key] for key in required_strings):
        raise RelayProtocolError("Relay returned invalid AITP challenge fields")
    amount = value.get("amount_micros")
    expiry = value.get("expiry")
    if (
        not isinstance(amount, int)
        or isinstance(amount, bool)
        or amount < 0
        or not isinstance(expiry, int)
        or isinstance(expiry, bool)
        or expiry < 0
        or value.get("currency") != "AIT"
        or value.get("scheme") != "aitp-hold"
    ):
        raise RelayProtocolError("Relay returned invalid AITP challenge fields")
    return AitpChallenge(
        payee=value["payee"],
        amount_micros=amount,
        currency="AIT",
        resource=value["resource"],
        nonce=value["nonce"],
        expiry=expiry,
        scheme="aitp-hold",
    )
