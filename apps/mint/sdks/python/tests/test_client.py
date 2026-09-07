import json
import unittest
from unittest.mock import patch

from railyard_mint import MintClient


def _transfer_proof(**body_overrides):
    body = {
        "account_id": "acct_sender",
        "account_seq": 3,
        "journal_seq": 7,
        "op": "Transfer",
        "op_hash": [17] * 32,
        "amount_micros": 2_000_000,
        "from": "acct_sender",
        "to": "acct_recipient",
        "balance_after_available_micros": 8_000_000,
        "balance_after_held_micros": 0,
        "ts": 1_700_000_000_000,
        "prev_hash": "00" * 32,
        "signer": "mint-key-1",
    }
    body.update(body_overrides)
    return {
        "receipt": {"body": body, "hash": "22" * 32, "sig": "33" * 64},
        "attestation": {
            "format": "mint-transfer-attestation-v1",
            "payload_base64url": "cGF5bG9hZA",
            "digest_sha256": "44" * 32,
            "signer_key_id": "mint-key-1",
            "signature_hex": "55" * 64,
        },
    }


class _Response:
    def __init__(self, body: dict, *, status: int = 200, headers=None) -> None:
        self.status = status
        self.headers = headers or {}
        self._body = json.dumps(body).encode()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self) -> bytes:
        return self._body


class MintClientTransferTests(unittest.TestCase):
    def test_transfer_sends_exact_integer_and_stable_key(self) -> None:
        captured = []

        def open_request(request, **_kwargs):
            captured.append(request)
            return _Response(
                {
                    "success": True,
                    "data": {
                        "first_seq": 7,
                        "from_balance": {"available_micros": 8_000_000},
                        "to_balance": {"available_micros": 2_000_000},
                        "idempotent_replay": False,
                        **_transfer_proof(),
                    },
                }
            )

        with patch("railyard_mint.client.urlopen", open_request):
            receipt = MintClient("https://mint.example", "secret").transfer(
                {
                    "from": "acct_sender",
                    "to": "acct_recipient",
                    "amount_micros": 2_000_000,
                    "idem_key": "person-send-1",
                    "ref": "menivor:transfer-1",
                }
            )

        self.assertEqual(receipt["first_seq"], 7)
        self.assertFalse(receipt["idempotent_replay"])
        self.assertEqual(
            json.loads(captured[0].data),
            {
                "from": "acct_sender",
                "to": "acct_recipient",
                "amount_micros": 2_000_000,
                "idem_key": "person-send-1",
                "ref": "menivor:transfer-1",
            },
        )

    def test_transfer_rejects_unsafe_money_before_transport(self) -> None:
        client = MintClient("https://mint.example", "secret")
        invalid_amounts = [True, 0, -1, 1.5]
        with patch("railyard_mint.client.urlopen") as open_request:
            for amount in invalid_amounts:
                with self.subTest(amount=amount), self.assertRaises(ValueError):
                    client.transfer(
                        {
                            "from": "acct_sender",
                            "to": "acct_recipient",
                            "amount_micros": amount,
                            "idem_key": "person-send-1",
                        }
                    )
            open_request.assert_not_called()

    def test_transfer_rejects_success_without_matching_signed_proof(self) -> None:
        cases = [
            {
                "first_seq": 7,
                "from_balance": {},
                "to_balance": {},
                "idempotent_replay": False,
            },
            {
                "first_seq": 7,
                "from_balance": {},
                "to_balance": {},
                "idempotent_replay": False,
                **_transfer_proof(amount_micros=1),
            },
            {
                "first_seq": 7,
                "from_balance": {},
                "to_balance": {},
                "idempotent_replay": False,
                **_transfer_proof(to="acct_attacker"),
            },
        ]
        for data in cases:
            with self.subTest(data=data), patch(
                "railyard_mint.client.urlopen",
                return_value=_Response({"success": True, "data": data}),
            ):
                with self.assertRaisesRegex(RuntimeError, "Mint transfer proof"):
                    MintClient("https://mint.example", "secret").transfer(
                        {
                            "from": "acct_sender",
                            "to": "acct_recipient",
                            "amount_micros": 2_000_000,
                            "idem_key": "person-send-1",
                        }
                    )

    def test_public_address_transfer_resolves_recipient_before_debit(self) -> None:
        requests = []

        def open_request(request, **_kwargs):
            requests.append(request)
            if len(requests) == 1:
                return _Response({"success": True, "data": {"account_id": "acct_recipient"}})
            return _Response(
                {
                    "success": True,
                    "data": {
                        "first_seq": 7,
                        "from_balance": {},
                        "to_balance": {},
                        "idempotent_replay": False,
                        **_transfer_proof(),
                    },
                }
            )

        with patch("railyard_mint.client.urlopen", open_request):
            MintClient("https://mint.example", "secret").transfer_to_address(
                from_account="acct_sender",
                address="ait1recipient",
                amount_micros=2_000_000,
                idem_key="address-send-1",
            )

        self.assertTrue(requests[0].full_url.endswith("/v1/addresses/ait1recipient"))
        self.assertEqual(json.loads(requests[1].data)["to"], "acct_recipient")

    def test_stream_receipts_reserve_and_revocation_are_exposed(self) -> None:
        requests = []

        def open_request(request, **_kwargs):
            requests.append(request)
            data = [] if request.full_url.endswith("/receipts") else {}
            return _Response({"success": True, "data": data})

        client = MintClient("https://mint.example", "secret")
        with patch("railyard_mint.client.urlopen", open_request):
            client.open_stream(
                {
                    "from": "acct_sender",
                    "to": "acct_recipient",
                    "rate_micros": 5,
                    "cap_micros": 100,
                    "unit": "Unit",
                    "idem_key": "open-1",
                }
            )
            client.revoke_allowance("allow_1", "revoke-1")
            client.account_receipts("acct_sender")
            client.reserve()

        self.assertEqual(
            [request.full_url.removeprefix("https://mint.example/") for request in requests],
            [
                "v1/streams",
                "v1/allowances/allow_1/revoke",
                "v1/accounts/acct_sender/receipts",
                "v1/reserve",
            ],
        )


if __name__ == "__main__":
    unittest.main()
