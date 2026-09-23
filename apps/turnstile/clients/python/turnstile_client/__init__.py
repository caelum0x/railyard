"""Turnstile agent-side payment client (AITP-402 over HTTP and MCP).

Standard library only. See ``client.py`` for the protocol walkthrough.
"""

from .client import (
    AITP_VERSION,
    Challenge,
    Ed25519Signer,
    HttpError,
    McpResult,
    PaidResponse,
    PaymentFailed,
    PaymentRefused,
    ProtocolError,
    Signer,
    SpendPolicy,
    TurnstileClient,
    TurnstileError,
    build_authorization,
    canonical_json,
    decode_receipt,
    encode_authorization,
)

__all__ = [
    "AITP_VERSION",
    "Challenge",
    "Ed25519Signer",
    "HttpError",
    "McpResult",
    "PaidResponse",
    "PaymentFailed",
    "PaymentRefused",
    "ProtocolError",
    "Signer",
    "SpendPolicy",
    "TurnstileClient",
    "TurnstileError",
    "build_authorization",
    "canonical_json",
    "decode_receipt",
    "encode_authorization",
]
