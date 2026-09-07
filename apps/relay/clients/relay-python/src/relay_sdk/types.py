from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, Protocol, TypedDict


class AitpChallengeDict(TypedDict):
    payee: str
    amount_micros: int
    currency: Literal["AIT"]
    resource: str
    nonce: str
    expiry: int
    scheme: Literal["aitp-hold"]


@dataclass(frozen=True, slots=True)
class AitpChallenge:
    payee: str
    amount_micros: int
    currency: Literal["AIT"]
    resource: str
    nonce: str
    expiry: int
    scheme: Literal["aitp-hold"]


@dataclass(frozen=True, slots=True)
class PaymentRequired:
    challenge: AitpChallenge
    request_id: str | None
    protocol_version: str | None


@dataclass(frozen=True, slots=True)
class SettlementReceiptProof:
    sequence: int
    operation: str
    operation_hash: str
    amount_micros: int
    balance_after_micros: int | None
    prev_hash: str | None
    receipt_hash: str
    signature: str


@dataclass(frozen=True, slots=True)
class SettlementEvidence:
    request_id: str
    principal: str
    receipt: SettlementReceiptProof
    settled_at_ms: int
    attestation: Any | None = None
    verified_at_ms: int | None = None


def settlement_evidence_from_value(value: Any) -> SettlementEvidence | None:
    if not isinstance(value, Mapping):
        return None
    receipt = value.get("receipt")
    if not isinstance(receipt, Mapping):
        return None
    string_fields = ("operation", "operation_hash", "receipt_hash", "signature")
    if not all(isinstance(receipt.get(field), str) for field in string_fields):
        return None
    if type(receipt.get("sequence")) is not int or type(receipt.get("amount_micros")) is not int:
        return None
    balance_after_micros = receipt.get("balance_after_micros")
    prev_hash = receipt.get("prev_hash")
    if (
        (balance_after_micros is not None and type(balance_after_micros) is not int)
        or (prev_hash is not None and not isinstance(prev_hash, str))
    ):
        return None
    request_id = value.get("request_id")
    principal = value.get("principal")
    settled_at_ms = value.get("settled_at_ms")
    verified_at_ms = value.get("verified_at_ms")
    if (
        not isinstance(request_id, str)
        or not request_id
        or not isinstance(principal, str)
        or not principal
        or type(settled_at_ms) is not int
        or (verified_at_ms is not None and type(verified_at_ms) is not int)
    ):
        return None
    return SettlementEvidence(
        request_id=request_id,
        principal=principal,
        receipt=SettlementReceiptProof(
            sequence=receipt["sequence"],
            operation=receipt["operation"],
            operation_hash=receipt["operation_hash"],
            amount_micros=receipt["amount_micros"],
            balance_after_micros=balance_after_micros,
            prev_hash=prev_hash,
            receipt_hash=receipt["receipt_hash"],
            signature=receipt["signature"],
        ),
        settled_at_ms=settled_at_ms,
        attestation=value.get("attestation"),
        verified_at_ms=verified_at_ms,
    )


class AitpAuthorizer(Protocol):
    def __call__(self, payment: PaymentRequired) -> str: ...


class AsyncAitpAuthorizer(Protocol):
    async def __call__(self, payment: PaymentRequired) -> str: ...


JsonObject = dict[str, Any]
