"""AITP-402 wire codec and one-retry paid HTTP flow."""

from __future__ import annotations

import base64
import json
import time
from dataclasses import asdict, dataclass
from typing import Mapping, Protocol
from urllib.error import HTTPError
from urllib.request import Request, urlopen

AITP_VERSION = "0.1"
HEADER_VERSION = "AITP-Version"
HEADER_CHALLENGE = "AITP-Challenge"
HEADER_AUTHORIZATION = "AITP-Authorization"
HEADER_RECEIPT = "AITP-Receipt"


@dataclass(frozen=True)
class PaymentChallenge:
    payee: str
    amount_micros: int
    currency: str
    resource: str
    nonce: str
    expiry: int
    scheme: str


@dataclass(frozen=True)
class PaymentAuthorization:
    payer: str
    payee: str
    amount_micros: int
    nonce: str
    resource: str
    expiry: int
    scheme: str
    idempotency_key: str
    pubkey_id: str
    op_hash: str
    sig: str


@dataclass(frozen=True)
class Receipt:
    seq: int
    op: str
    op_hash: str
    amount_micros: int
    balance_after_micros: int
    prev_hash: str
    hash: str
    sig: str


@dataclass(frozen=True)
class SignResult:
    pubkey_id: str
    op_hash: str
    sig: str


class WalletSigner(Protocol):
    """Wallet/HSM seam; signs BLAKE3(canonical_intent) with Ed25519."""

    def sign_authorization(self, canonical_intent: bytes) -> SignResult: ...


class AuthorizationBuilder:
    def __init__(self, payer: str, signer: WalletSigner) -> None:
        self.payer = payer
        self.signer = signer

    def authorize(self, challenge: PaymentChallenge) -> PaymentAuthorization:
        _validate_challenge(challenge)
        if challenge.expiry <= int(time.time() * 1000):
            raise ValueError("AITP challenge is expired")
        idempotency_key = f"aitp:{challenge.nonce}"
        intent = {
            "payer": self.payer,
            "payee": challenge.payee,
            "amount_micros": challenge.amount_micros,
            "nonce": challenge.nonce,
            "resource": challenge.resource,
            "expiry": challenge.expiry,
            "scheme": challenge.scheme,
            "idempotency_key": idempotency_key,
        }
        canonical = json.dumps(
            intent, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode("utf-8")
        signed = self.signer.sign_authorization(canonical)
        if len(signed.op_hash) != 64 or any(c not in "0123456789abcdef" for c in signed.op_hash):
            raise ValueError("wallet signer returned a non-canonical BLAKE3 hex digest")
        if not signed.pubkey_id or not signed.sig:
            raise ValueError("wallet signer returned an empty key id or signature")
        return PaymentAuthorization(
            payer=self.payer,
            payee=challenge.payee,
            amount_micros=challenge.amount_micros,
            nonce=challenge.nonce,
            resource=challenge.resource,
            expiry=challenge.expiry,
            scheme=challenge.scheme,
            idempotency_key=idempotency_key,
            pubkey_id=signed.pubkey_id,
            op_hash=signed.op_hash,
            sig=signed.sig,
        )


@dataclass(frozen=True)
class HttpRequest:
    method: str
    url: str
    headers: Mapping[str, str]
    body: bytes = b""

    def with_header(self, name: str, value: str) -> HttpRequest:
        headers = dict(self.headers)
        headers[name] = value
        return HttpRequest(self.method, self.url, headers, self.body)


@dataclass(frozen=True)
class HttpResponse:
    status: int
    headers: Mapping[str, str]
    body: bytes

    def header(self, name: str) -> str | None:
        target = name.lower()
        return next((value for key, value in self.headers.items() if key.lower() == target), None)


class HttpTransport(Protocol):
    def send(self, request: HttpRequest) -> HttpResponse: ...


class StdlibTransport:
    def __init__(self, *, timeout: float = 120.0) -> None:
        self.timeout = timeout

    def send(self, request: HttpRequest) -> HttpResponse:
        wire = Request(
            request.url,
            method=request.method,
            headers=dict(request.headers),
            data=request.body or None,
        )
        try:
            with urlopen(wire, timeout=self.timeout) as response:
                return HttpResponse(response.status, dict(response.headers), response.read())
        except HTTPError as error:
            return HttpResponse(error.code, dict(error.headers), error.read())


class PaidHttpClient:
    def __init__(self, transport: HttpTransport, authorizer: AuthorizationBuilder) -> None:
        self.transport = transport
        self.authorizer = authorizer

    def send(self, request: HttpRequest) -> HttpResponse:
        first = self.transport.send(request)
        if first.status != 402:
            return first
        encoded = first.header(HEADER_CHALLENGE)
        if encoded is None:
            raise ValueError("402 response missing AITP-Challenge header")
        authorization = self.authorizer.authorize(decode_challenge(encoded))
        retry = request.with_header(HEADER_VERSION, AITP_VERSION).with_header(
            HEADER_AUTHORIZATION, encode_authorization(authorization)
        )
        return self.transport.send(retry)


def encode_authorization(value: PaymentAuthorization) -> str:
    return _encode(asdict(value))


def decode_authorization(header: str) -> PaymentAuthorization:
    return PaymentAuthorization(**_decode(header))


def decode_challenge(header: str) -> PaymentChallenge:
    challenge = PaymentChallenge(**_decode(header))
    _validate_challenge(challenge)
    return challenge


def decode_receipt(header: str) -> Receipt:
    return Receipt(**_decode(header))


def _encode(value: Mapping[str, object]) -> str:
    raw = json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _decode(header: str) -> dict[str, object]:
    encoded = header.strip().encode("ascii")
    encoded += b"=" * (-len(encoded) % 4)
    value = json.loads(base64.urlsafe_b64decode(encoded))
    if not isinstance(value, dict):
        raise ValueError("AITP header must encode a JSON object")
    return value


def _validate_challenge(challenge: PaymentChallenge) -> None:
    if challenge.currency != "AIT":
        raise ValueError("AITP currency must be AIT")
    if challenge.scheme not in {"aitp-hold", "aitp-transfer"}:
        raise ValueError("unsupported AITP scheme")
    if challenge.amount_micros < 0:
        raise ValueError("AITP amount_micros cannot be negative")
    if not challenge.payee or not challenge.resource or not challenge.nonce:
        raise ValueError("AITP challenge identity fields cannot be empty")
