from __future__ import annotations

import base64
import json
import time
import unittest

from railyard_mint import (
    AuthorizationBuilder,
    HttpRequest,
    HttpResponse,
    PaidHttpClient,
    SignResult,
    decode_authorization,
)


class FakeSigner:
    def sign_authorization(self, canonical_intent: bytes) -> SignResult:
        self.canonical = canonical_intent
        return SignResult("key_test", "07" * 32, "fake-ed25519-signature")


class ChallengeThenOk:
    def __init__(self) -> None:
        self.requests: list[HttpRequest] = []

    def send(self, request: HttpRequest) -> HttpResponse:
        self.requests.append(request)
        if len(self.requests) == 1:
            challenge = {
                "payee": "ait1qrelay",
                "amount_micros": 4200,
                "currency": "AIT",
                "resource": "POST /v1/chat#model",
                "nonce": "nonce-1",
                "expiry": int(time.time() * 1000) + 60_000,
                "scheme": "aitp-hold",
            }
            raw = json.dumps(challenge, separators=(",", ":")).encode()
            encoded = base64.urlsafe_b64encode(raw).rstrip(b"=").decode()
            return HttpResponse(402, {"AITP-Challenge": encoded}, b"")
        return HttpResponse(200, {}, b'{"ok":true}')


class PaidFlowTest(unittest.TestCase):
    def test_signs_and_retries_once_without_changing_body(self) -> None:
        transport = ChallengeThenOk()
        signer = FakeSigner()
        client = PaidHttpClient(
            transport, AuthorizationBuilder("acct_payer", signer)
        )
        response = client.send(
            HttpRequest("POST", "https://relay.test/v1/chat", {}, b'{"model":"x"}')
        )
        self.assertEqual(response.status, 200)
        self.assertEqual(len(transport.requests), 2)
        self.assertEqual(transport.requests[0].body, transport.requests[1].body)
        encoded = next(
            value
            for key, value in transport.requests[1].headers.items()
            if key.lower() == "aitp-authorization"
        )
        auth = decode_authorization(encoded)
        self.assertEqual(auth.payer, "acct_payer")
        self.assertEqual(auth.scheme, "aitp-hold")
        self.assertEqual(auth.idempotency_key, "aitp:nonce-1")
        self.assertEqual(
            list(json.loads(signer.canonical).keys()),
            sorted(json.loads(signer.canonical).keys()),
        )


if __name__ == "__main__":
    unittest.main()
