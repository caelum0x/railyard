import base64
import json

import pytest

from relay_sdk import RelayProtocolError, decode_aitp_challenge, payment_required_from_headers

REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002"


def challenge() -> dict[str, object]:
    return {
        "payee": "ait1qrelay",
        "amount_micros": 7_500,
        "currency": "AIT",
        "resource": f"POST /inference#m@{REQUEST_ID}",
        "nonce": "nonce",
        "expiry": 1_800_000_000_000,
        "scheme": "aitp-hold",
    }


def encoded_challenge() -> str:
    return base64.urlsafe_b64encode(json.dumps(challenge()).encode()).decode().rstrip("=")


def test_challenge_and_payment_headers_are_typed() -> None:
    decoded = decode_aitp_challenge(encoded_challenge())
    assert decoded.amount_micros == 7_500
    payment = payment_required_from_headers(
        {
            "AITP-Challenge": encoded_challenge(),
            "AITP-Request-ID": REQUEST_ID,
            "AITP-Version": "1",
        }
    )
    assert payment.challenge == decoded
    assert payment.request_id == REQUEST_ID
    assert payment.protocol_version == "1"


@pytest.mark.parametrize(
    "encoded",
    ["not base64!", base64.urlsafe_b64encode(b"[]").decode().rstrip("=")],
)
def test_invalid_challenge_fails_closed(encoded: str) -> None:
    with pytest.raises(RelayProtocolError):
        decode_aitp_challenge(encoded)
