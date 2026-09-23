"""Synchronous standard-library client for Mint's stable HTTP surface."""

from __future__ import annotations

import json
from dataclasses import asdict
from typing import Any, Mapping, NotRequired, TypedDict, cast
from urllib.error import HTTPError
from urllib.parse import quote, urlencode, urljoin, urlparse
from urllib.request import Request, urlopen


class MintApiError(RuntimeError):
    def __init__(self, status: int, code: str, message: str, body: Any) -> None:
        super().__init__(f"Mint returned HTTP {status} ({code}): {message}")
        self.status = status
        self.code = code
        self.body = body


TransferRequest = TypedDict(
    "TransferRequest",
    {
        "from": str,
        "to": str,
        "amount_micros": int,
        "idem_key": str,
        "ref": NotRequired[str],
    },
)


class TransferReceipt(TypedDict):
    first_seq: int
    from_balance: dict[str, Any]
    to_balance: dict[str, Any]
    idempotent_replay: bool
    receipt: SignedTransferReceipt
    attestation: TransferAttestation


TransferReceiptBody = TypedDict(
    "TransferReceiptBody",
    {
        "account_id": str,
        "account_seq": int,
        "journal_seq": int,
        "op": str,
        "op_hash": list[int],
        "amount_micros": int,
        "from": str | None,
        "to": str | None,
        "balance_after_available_micros": int,
        "balance_after_held_micros": int,
        "ts": int,
        "prev_hash": str,
        "signer": str,
    },
)


class SignedTransferReceipt(TypedDict):
    body: TransferReceiptBody
    hash: str
    sig: str


class TransferAttestation(TypedDict):
    format: str
    payload_base64url: str
    digest_sha256: str
    signer_key_id: str
    signature_hex: str


class MintClient:
    def __init__(self, base_url: str, api_key: str, *, timeout: float = 30.0) -> None:
        parsed = urlparse(base_url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc:
            raise ValueError("base_url must be an absolute HTTP(S) URL")
        self.base_url = base_url.rstrip("/") + "/"
        self.api_key = api_key
        self.timeout = timeout

    def create_account(self, kind: str, owner_ref: str | None = None) -> dict[str, Any]:
        return self._request("POST", "v1/accounts", {"kind": kind, "owner_ref": owner_ref})

    def account(self, account_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/accounts/{_segment(account_id)}")

    def balance(self, account_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/accounts/{_segment(account_id)}/balance")

    def create_address(self, account_id: str, purpose: str | None = None) -> dict[str, Any]:
        return self._request(
            "POST",
            f"v1/accounts/{_segment(account_id)}/addresses",
            {"purpose": purpose},
        )

    def lookup_address(self, address: str) -> dict[str, Any]:
        return self._request("GET", f"v1/addresses/{_segment(address)}")

    def create_checkout(
        self, provider: str, account_id: str, amount_micros: int
    ) -> dict[str, Any]:
        return self._request(
            "POST",
            "v1/checkouts",
            {
                "provider": provider,
                "account_id": account_id,
                "amount_micros": amount_micros,
            },
        )

    def transfer(self, request: Mapping[str, Any]) -> TransferReceipt:
        """Move integer microdollars exactly once for one logical operation.

        This account-id API is intended for trusted services. Consumer products
        should resolve people to accounts server-side and must never expose Mint
        account ids or the service API key to a browser/mobile client.
        """
        _validate_transfer(request)
        if cast(str, request["to"]).startswith("ait1"):
            raise ValueError(
                "public Mint addresses must use transfer_to_address so the signed "
                "receipt can be bound to the resolved recipient"
            )
        data = self._request("POST", "v1/transfers", request)
        _validate_transfer_receipt(data, request)
        return cast(TransferReceipt, data)

    def transfer_to_address(
        self,
        *,
        from_account: str,
        address: str,
        amount_micros: int,
        idem_key: str,
        reference: str | None = None,
    ) -> TransferReceipt:
        """Resolve a public receive address and transfer to its bound account."""
        if not address.startswith("ait1"):
            raise ValueError("public Mint addresses must begin with ait1")
        recipient = self.lookup_address(address)
        account_id = recipient.get("account_id")
        if not isinstance(account_id, str) or not account_id:
            raise MintApiError(200, "invalid_response", "Mint address lookup is invalid", recipient)
        request: dict[str, Any] = {
            "from": from_account,
            "to": account_id,
            "amount_micros": amount_micros,
            "idem_key": idem_key,
        }
        if reference is not None:
            request["ref"] = reference
        return self.transfer(request)

    def create_hold(self, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._request("POST", "v1/holds", request)

    def hold(self, hold_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/holds/{_segment(hold_id)}")

    def capture_hold(self, hold_id: str, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._request("POST", f"v1/holds/{_segment(hold_id)}/capture", request)

    def release_hold(self, hold_id: str, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._request("POST", f"v1/holds/{_segment(hold_id)}/release", request)

    def grant_allowance(self, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._request("POST", "v1/allowances", request)

    def allowance(self, allowance_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/allowances/{_segment(allowance_id)}")

    def spend_allowance(self, allowance_id: str, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._request("POST", f"v1/allowances/{_segment(allowance_id)}/spend", request)

    def revoke_allowance(self, allowance_id: str, idem_key: str) -> dict[str, Any]:
        return self._request(
            "POST",
            f"v1/allowances/{_segment(allowance_id)}/revoke",
            {"idem_key": idem_key},
        )

    def open_stream(self, request: Mapping[str, Any]) -> dict[str, Any]:
        _validate_positive_money("stream rate_micros", request.get("rate_micros"))
        _validate_positive_money("stream cap_micros", request.get("cap_micros"))
        return self._request("POST", "v1/streams", request)

    def stream(self, stream_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/streams/{_segment(stream_id)}")

    def tick_stream(self, stream_id: str, request: Mapping[str, Any]) -> dict[str, Any]:
        _validate_counter("stream units", request.get("units"))
        _validate_counter("stream seq", request.get("seq"))
        return self._request("POST", f"v1/streams/{_segment(stream_id)}/tick", request)

    def close_stream(self, stream_id: str, request: Mapping[str, Any]) -> dict[str, Any]:
        _validate_counter("stream units", request.get("units"))
        _validate_counter("stream seq", request.get("seq"))
        return self._request("POST", f"v1/streams/{_segment(stream_id)}/close", request)

    def account_receipts(self, account_id: str) -> list[dict[str, Any]]:
        return self._request("GET", f"v1/accounts/{_segment(account_id)}/receipts")

    def receipt_audit(self) -> dict[str, Any]:
        return self._request("GET", "v1/receipts/audit")

    def reserve(self) -> dict[str, Any]:
        return self._request("GET", "v1/reserve")

    def mint(self, request: Mapping[str, Any]) -> dict[str, Any]:
        _validate_positive_money("mint amount_micros", request.get("amount_micros"))
        return self._request("POST", "v1/mint", request)

    def redeem(self, request: Mapping[str, Any]) -> dict[str, Any]:
        _validate_positive_money("redeem amount_micros", request.get("amount_micros"))
        return self._request("POST", "v1/redeem", request)

    # ---- accounts: listing, statements, KYC, lifecycle -------------------

    def list_accounts(self, *, after: str | None = None, limit: int | None = None) -> dict[str, Any]:
        """Operator listing of accounts with live balances (keyset paginated)."""
        return self._request("GET", f"v1/accounts{_query(after=after, limit=limit)}")

    def statement(
        self, account_id: str, *, from_ms: int | None = None, to_ms: int | None = None
    ) -> dict[str, Any]:
        return self._request(
            "GET",
            f"v1/accounts/{_segment(account_id)}/statement{_query(from_ms=from_ms, to_ms=to_ms)}",
        )

    def kyc_status(self, account_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/accounts/{_segment(account_id)}/kyc")

    def start_kyc_session(self, account_id: str) -> dict[str, Any]:
        return self._request("POST", f"v1/accounts/{_segment(account_id)}/kyc/session", {})

    def freeze_account(self, account_id: str) -> dict[str, Any]:
        """Block outgoing movement from an account (operator key)."""
        return self._request("POST", f"v1/accounts/{_segment(account_id)}/freeze", {})

    def unfreeze_account(self, account_id: str) -> dict[str, Any]:
        return self._request("POST", f"v1/accounts/{_segment(account_id)}/unfreeze", {})

    def close_account(self, account_id: str) -> dict[str, Any]:
        """Permanently close a zero-balance account (operator key)."""
        return self._request("POST", f"v1/accounts/{_segment(account_id)}/close", {})

    # ---- payouts --------------------------------------------------------

    def list_payouts(
        self,
        *,
        status: str | None = None,
        account_id: str | None = None,
        limit: int | None = None,
        cursor: str | None = None,
    ) -> dict[str, Any]:
        return self._request(
            "GET",
            f"v1/payouts{_query(status=status, account_id=account_id, limit=limit, cursor=cursor)}",
        )

    def payout(self, payout_ref: str) -> dict[str, Any]:
        """One payout; a pending payout is refreshed from the provider first."""
        return self._request("GET", f"v1/payouts/{_segment(payout_ref)}")

    def retry_payout(self, payout_ref: str) -> dict[str, Any]:
        return self._request("POST", f"v1/payouts/{_segment(payout_ref)}/retry", {})

    def cancel_payout(self, payout_ref: str, note: str | None = None) -> dict[str, Any]:
        """Abandon a failed payout and re-credit the burned amount (operator key)."""
        return self._request("POST", f"v1/payouts/{_segment(payout_ref)}/cancel", {"note": note})

    def mark_payout_paid(self, payout_ref: str, provider_ref: str | None = None) -> dict[str, Any]:
        """Record an out-of-band settlement of a payout (operator key)."""
        return self._request(
            "POST", f"v1/payouts/{_segment(payout_ref)}/mark-paid", {"provider_ref": provider_ref}
        )

    # ---- reconciliation -------------------------------------------------

    def reconciliation(self) -> dict[str, Any]:
        return self._request("GET", "v1/reconciliation")

    def import_provider_report(
        self,
        provider: str,
        content: str,
        *,
        label: str | None = None,
        format: str | None = None,
        amount_unit: str | None = None,
        reference_column: str | None = None,
    ) -> dict[str, Any]:
        """Import a provider settlement report (CSV / JSON) and get its match results."""
        if not content:
            raise ValueError("report content cannot be empty")
        body: dict[str, Any] = {"provider": provider, "content": content}
        for key, value in (
            ("label", label),
            ("format", format),
            ("amount_unit", amount_unit),
            ("reference_column", reference_column),
        ):
            if value is not None:
                body[key] = value
        return self._request("POST", "v1/reconciliation/reports", body)

    def provider_reports(self) -> dict[str, Any]:
        return self._request("GET", "v1/reconciliation/reports")

    def provider_report(self, report_id: str) -> dict[str, Any]:
        return self._request("GET", f"v1/reconciliation/reports/{_segment(report_id)}")

    def delete_provider_report(self, report_id: str) -> dict[str, Any]:
        return self._request("DELETE", f"v1/reconciliation/reports/{_segment(report_id)}")

    # ---- deposit reversals ----------------------------------------------

    def record_reversal(
        self,
        deposit_ref: str,
        *,
        amount_micros: int | None = None,
        reason: str = "refund",
        note: str | None = None,
    ) -> dict[str, Any]:
        """Record a refund / chargeback and claw it back (operator key)."""
        body: dict[str, Any] = {"deposit_ref": deposit_ref, "reason": reason, "note": note}
        if amount_micros is not None:
            _validate_positive_money("reversal amount_micros", amount_micros)
            body["amount_micros"] = amount_micros
        return self._request("POST", "v1/reversals", body)

    def reversals(self, *, status: str | None = None, limit: int | None = None) -> dict[str, Any]:
        return self._request("GET", f"v1/reversals{_query(status=status, limit=limit)}")

    def retry_reversal(self, deposit_ref: str) -> dict[str, Any]:
        return self._request("POST", f"v1/reversals/{_segment(deposit_ref)}/retry", {})

    def write_off_reversal(self, deposit_ref: str, note: str | None = None) -> dict[str, Any]:
        return self._request(
            "POST", f"v1/reversals/{_segment(deposit_ref)}/write-off", {"note": note}
        )

    # ---- API keys -------------------------------------------------------

    def api_keys(self) -> dict[str, Any]:
        return self._request("GET", "v1/keys")

    def create_api_key(
        self, name: str, scopes: list[str], *, accounts: list[str] | None = None
    ) -> dict[str, Any]:
        """Create a DB-backed key. The response `secret` is returned ONCE."""
        return self._request(
            "POST", "v1/keys", {"name": name, "scopes": scopes, "accounts": accounts}
        )

    def revoke_api_key(self, key_id: str) -> dict[str, Any]:
        return self._request("POST", f"v1/keys/{_segment(key_id)}/revoke", {})

    def verification_keys(self) -> dict[str, Any]:
        return self._request("GET", ".well-known/mint-verification-keys.json")

    def issue_aitp_challenge(self, request: Mapping[str, Any]):
        """Issue and cross-check the JSON and header copies of a 402 challenge."""
        from .aitp import HEADER_CHALLENGE, PaymentChallenge, decode_challenge

        _validate_positive_money("challenge amount_micros", request.get("amount_micros"))
        _validate_positive_money("challenge ttl_ms", request.get("ttl_ms"))
        wire = Request(
            urljoin(self.base_url, "v1/aitp/challenge"),
            data=json.dumps(request, separators=(",", ":")).encode(),
            method="POST",
            headers={
                "Accept": "application/json",
                "Content-Type": "application/json",
                "X-API-Key": self.api_key,
            },
        )
        try:
            with urlopen(wire, timeout=self.timeout) as response:
                status, headers, raw = response.status, dict(response.headers), response.read()
        except HTTPError as error:
            status, headers, raw = error.code, dict(error.headers), error.read()
        payload = json.loads(raw)
        if status != 402 or not isinstance(payload, dict):
            raise _api_error(status, payload)
        encoded = _header(headers, HEADER_CHALLENGE)
        if encoded is None:
            raise MintApiError(402, "invalid_response", "Mint challenge header is missing", payload)
        header = decode_challenge(encoded)
        body = PaymentChallenge(**payload)
        if asdict(header) != asdict(body):
            raise MintApiError(
                402, "invalid_response", "Mint challenge header and body disagree", payload
            )
        return body

    def settle_aitp(self, authorization) -> dict[str, Any]:
        """Submit a locally signed payer authorization and retain its receipt."""
        from .aitp import HEADER_RECEIPT, decode_receipt

        body = asdict(authorization) if hasattr(authorization, "__dataclass_fields__") else authorization
        data, headers = self._request_with_headers("POST", "v1/aitp/verify", body)
        encoded = _header(headers, HEADER_RECEIPT)
        if encoded is None:
            raise MintApiError(200, "invalid_response", "Mint settlement receipt is missing", data)
        return {**data, "signed_receipt": asdict(decode_receipt(encoded))}

    def _request(
        self, method: str, path: str, body: Mapping[str, Any] | None = None
    ) -> Any:
        return self._request_with_headers(method, path, body)[0]

    def _request_with_headers(
        self, method: str, path: str, body: Mapping[str, Any] | None = None
    ) -> tuple[Any, Mapping[str, str]]:
        encoded = None if body is None else json.dumps(body, separators=(",", ":")).encode()
        request = Request(
            urljoin(self.base_url, path),
            data=encoded,
            method=method,
            headers={
                "Accept": "application/json",
                "X-API-Key": self.api_key,
                **({"Content-Type": "application/json"} if encoded is not None else {}),
            },
        )
        try:
            with urlopen(request, timeout=self.timeout) as response:
                status = response.status
                headers = dict(response.headers)
                raw = response.read()
        except HTTPError as error:
            status = error.code
            headers = dict(error.headers)
            raw = error.read()
        try:
            payload = json.loads(raw)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise MintApiError(status, "invalid_response", str(error), raw) from error
        if status < 200 or status >= 300 or payload.get("success") is not True:
            raise _api_error(status, payload)
        data = payload.get("data")
        if not isinstance(data, (dict, list)):
            raise MintApiError(
                status, "invalid_response", "Mint data must be an object or array", payload
            )
        return data, headers


def _segment(value: str) -> str:
    return quote(value, safe="")


def _query(**params: Any) -> str:
    pairs = {k: str(v) for k, v in params.items() if v is not None and v != ""}
    return f"?{urlencode(pairs)}" if pairs else ""


def _header(headers: Mapping[str, str], name: str) -> str | None:
    target = name.lower()
    return next((value for key, value in headers.items() if key.lower() == target), None)


def _api_error(status: int, payload: Any) -> MintApiError:
    detail = payload.get("error") if isinstance(payload, dict) else None
    code = detail.get("code", "http_error") if isinstance(detail, dict) else "http_error"
    message = (
        detail.get("message", "Mint request failed")
        if isinstance(detail, dict)
        else "Mint request failed"
    )
    return MintApiError(status, code, message, payload)


def _validate_transfer(request: Mapping[str, Any]) -> None:
    for field in ("from", "to", "idem_key"):
        value = request.get(field)
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"transfer {field} must be a non-empty string")
    amount = request.get("amount_micros")
    if isinstance(amount, bool) or not isinstance(amount, int) or amount <= 0:
        raise ValueError("transfer amount_micros must be a positive integer")
    if len(cast(str, request["idem_key"])) > 128:
        raise ValueError("transfer idem_key must be at most 128 characters")
    reference = request.get("ref")
    if reference is not None and not isinstance(reference, str):
        raise ValueError("transfer ref must be a string when provided")


def _validate_positive_money(name: str, value: Any) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{name} must be a positive integer")


def _validate_counter(name: str, value: Any) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{name} must be a non-negative integer")


def _validate_transfer_receipt(
    value: Mapping[str, Any], request: Mapping[str, Any]
) -> None:
    """Require Mint's complete signed proof and bind it to this command."""
    first_seq = value.get("first_seq")
    receipt = value.get("receipt")
    attestation = value.get("attestation")
    if (
        isinstance(first_seq, bool)
        or not isinstance(first_seq, int)
        or first_seq < 0
        or not isinstance(value.get("from_balance"), dict)
        or not isinstance(value.get("to_balance"), dict)
        or not isinstance(value.get("idempotent_replay"), bool)
        or not isinstance(receipt, dict)
        or not isinstance(attestation, dict)
    ):
        raise MintApiError(200, "invalid_response", "Mint transfer proof is incomplete", value)

    body = receipt.get("body")
    if not isinstance(body, dict):
        raise MintApiError(200, "invalid_response", "Mint transfer receipt body is missing", value)
    strings = (
        receipt.get("hash"),
        receipt.get("sig"),
        body.get("op"),
        body.get("prev_hash"),
        body.get("signer"),
        attestation.get("payload_base64url"),
        attestation.get("digest_sha256"),
        attestation.get("signer_key_id"),
        attestation.get("signature_hex"),
    )
    if any(not isinstance(item, str) or not item for item in strings):
        raise MintApiError(200, "invalid_response", "Mint transfer proof fields are invalid", value)
    if attestation.get("format") != "mint-transfer-attestation-v1":
        raise MintApiError(200, "invalid_response", "Mint transfer attestation format is invalid", value)

    expected_amount = request["amount_micros"]
    if (
        body.get("account_id") != request["from"]
        or body.get("from") != request["from"]
        or body.get("to") != request["to"]
        or body.get("journal_seq") != first_seq
        or body.get("amount_micros") != expected_amount
        or body.get("op") not in {"Transfer", "transfer"}
    ):
        raise MintApiError(200, "invalid_response", "Mint transfer proof does not match request", value)
