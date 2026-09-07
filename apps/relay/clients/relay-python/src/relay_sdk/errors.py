from __future__ import annotations

from typing import Any

from .types import PaymentRequired, SettlementEvidence


class RelayError(Exception):
    """Base SDK error."""


class RelayConfigurationError(RelayError, ValueError):
    pass


class RelayProtocolError(RelayError):
    pass


class RelayTransportError(RelayError):
    pass


class RelayResponseTooLarge(RelayProtocolError):
    def __init__(self, limit: int) -> None:
        super().__init__(f"Relay response exceeded {limit} bytes")
        self.limit = limit


class RelayApiError(RelayError):
    def __init__(
        self,
        message: str,
        *,
        status: int,
        error_type: str | None = None,
        code: str | None = None,
        body: Any = None,
    ) -> None:
        super().__init__(message)
        self.status = status
        self.error_type = error_type
        self.code = code
        self.body = body


class RelayAuthenticationError(RelayApiError):
    pass


class RelayPaymentRequiredError(RelayApiError):
    def __init__(self, payment: PaymentRequired, *, body: Any = None) -> None:
        super().__init__(
            "Relay requires AITP payment",
            status=402,
            error_type="payment_required",
            code="aitp_payment_required",
            body=body,
        )
        self.payment = payment


class RelayAlreadySettledError(RelayApiError):
    def __init__(self, evidence: SettlementEvidence, *, body: Any = None) -> None:
        super().__init__(
            "Relay request already settled",
            status=409,
            error_type="request_already_settled",
            code="request_already_settled",
            body=body,
        )
        self.evidence = evidence


class RelayUnverifiedSettlementEvidenceError(RelayApiError):
    def __init__(self, evidence: SettlementEvidence, *, body: Any = None) -> None:
        super().__init__(
            "Relay returned settlement evidence but no pinned Mint verifier is configured",
            status=409,
            error_type="unverified_settlement_evidence",
            code="unverified_settlement_evidence",
            body=body,
        )
        self.evidence = evidence


class RelayInvalidSettlementEvidenceError(RelayApiError):
    def __init__(self, evidence: SettlementEvidence, *, body: Any = None) -> None:
        super().__init__(
            "Relay settlement evidence failed independent Mint verification",
            status=409,
            error_type="invalid_settlement_evidence",
            code="invalid_settlement_evidence",
            body=body,
        )
        self.evidence = evidence
