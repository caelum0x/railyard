"""Agent-side AITP-402 payment client for Turnstile-gated endpoints.

Flow for one paid call (HTTP or MCP):

1. Send the request unpaid.
2. Turnstile answers ``402`` with an ``AITP-Challenge`` header (HTTP) or a
   JSON-RPC error ``-32402`` whose ``data.aitp_challenge`` carries it (MCP).
3. The client checks the challenge against its spending policy (price cap,
   allowed payees, expiry), builds the authorization intent, hashes it
   (BLAKE3 over sorted-key canonical JSON, exactly like Mint) and signs the
   32-byte hash with the payer's Ed25519 key.
4. It retries with ``AITP-Authorization`` (HTTP) or the reserved
   ``_aitp_authorization`` tool argument (MCP). Turnstile asks Mint to verify
   the signature and open the hold, runs the tool, then captures actual usage
   (success-only) and returns the signed ``AITP-Receipt``.
5. ``settlement`` / ``wait_for_settlement`` poll
   ``GET /v1/settlements/:request_id`` (scoped by the same authorization) when
   the response reported ``capture_pending`` / ``release_pending``.

The client never talks to Mint directly and never holds money: Mint opens the
hold when Turnstile verifies the authorization. Only the Python standard
library is required.
"""

from __future__ import annotations

import base64
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Iterable, List, Mapping, Optional, Protocol

from . import _ed25519
from ._blake3 import blake3_digest

AITP_VERSION = "0.1"
MCP_PAYMENT_REQUIRED = -32402
AUTHORIZATION_ARG = "_aitp_authorization"
TERMINAL_PHASES = frozenset({"captured", "released", "transfer_complete", "rejected"})


class TurnstileError(Exception):
    """Base error for the Turnstile client."""


class PaymentRefused(TurnstileError):
    """The client's spending policy refused to pay a challenge."""


class PaymentFailed(TurnstileError):
    """Turnstile rejected the signed authorization (it re-challenged)."""

    def __init__(self, message: str, reason: Optional[str] = None):
        super().__init__(message)
        self.reason = reason


class ProtocolError(TurnstileError):
    """The server's response did not follow the AITP-402 protocol."""


class HttpError(TurnstileError):
    """A non-payment HTTP failure."""

    def __init__(self, status: int, body: bytes):
        super().__init__(f"HTTP {status}: {body[:300].decode('utf-8', 'replace')}")
        self.status = status
        self.body = body


def b64url_encode(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def b64url_decode(text: str) -> bytes:
    text = text.strip()
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def canonical_json(value: Mapping[str, Any]) -> bytes:
    """Sorted-key compact JSON, byte-compatible with Mint's canonical form."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode(
        "utf-8"
    )


@dataclass(frozen=True)
class Challenge:
    payee: str
    amount_micros: int
    currency: str
    resource: str
    nonce: str
    expiry: int
    scheme: str
    encoded: str

    @classmethod
    def decode(cls, header: str) -> "Challenge":
        try:
            data = json.loads(b64url_decode(header))
            return cls(
                payee=str(data["payee"]),
                amount_micros=int(data["amount_micros"]),
                currency=str(data["currency"]),
                resource=str(data["resource"]),
                nonce=str(data["nonce"]),
                expiry=int(data["expiry"]),
                scheme=str(data["scheme"]),
                encoded=header.strip(),
            )
        except (ValueError, KeyError, TypeError) as exc:
            raise ProtocolError(f"malformed AITP-Challenge: {exc}") from exc


class Signer(Protocol):
    """Anything that can sign a 32-byte op_hash for a registered Mint key."""

    @property
    def pubkey_id(self) -> str: ...

    def sign(self, op_hash: bytes) -> bytes: ...


class Ed25519Signer:
    """Signs with a 32-byte Ed25519 seed. Register ``public_key_hex`` with Mint
    under ``(payer, pubkey_id)`` before paying."""

    def __init__(self, pubkey_id: str, seed: bytes):
        if not pubkey_id:
            raise ValueError("pubkey_id is required")
        self._pubkey_id = pubkey_id
        self._seed = bytes(seed)
        self.public_key = _ed25519.public_key(self._seed)

    @classmethod
    def from_seed_hex(cls, pubkey_id: str, seed_hex: str) -> "Ed25519Signer":
        return cls(pubkey_id, bytes.fromhex(seed_hex.strip()))

    @property
    def pubkey_id(self) -> str:
        return self._pubkey_id

    @property
    def public_key_hex(self) -> str:
        return self.public_key.hex()

    def sign(self, op_hash: bytes) -> bytes:
        return _ed25519.sign(self._seed, op_hash)


@dataclass
class SpendPolicy:
    """What the agent is willing to pay. Every challenge is checked before
    signing; a refusal raises :class:`PaymentRefused` and nothing is signed."""

    max_price_micros: int
    allowed_payees: Optional[Iterable[str]] = None
    allowed_currencies: Iterable[str] = ("AIT",)
    total_budget_micros: Optional[int] = None
    spent_micros: int = 0

    def check(self, challenge: Challenge) -> None:
        if challenge.amount_micros <= 0:
            raise PaymentRefused("challenge quotes a non-positive amount")
        if challenge.amount_micros > self.max_price_micros:
            raise PaymentRefused(
                f"price {challenge.amount_micros} micros exceeds the cap {self.max_price_micros}"
            )
        if self.allowed_payees is not None and challenge.payee not in set(self.allowed_payees):
            raise PaymentRefused(f"payee {challenge.payee} is not allowed")
        if challenge.currency not in set(self.allowed_currencies):
            raise PaymentRefused(f"currency {challenge.currency} is not allowed")
        if challenge.expiry <= int(time.time() * 1000):
            raise PaymentRefused("challenge already expired")
        if (
            self.total_budget_micros is not None
            and self.spent_micros + challenge.amount_micros > self.total_budget_micros
        ):
            raise PaymentRefused("total budget exhausted")

    def record(self, micros: int) -> None:
        self.spent_micros += max(0, micros)


def build_authorization(payer: str, signer: Signer, challenge: Challenge) -> Dict[str, Any]:
    """Sign ``challenge`` for ``payer``; returns the authorization object."""
    idempotency_key = f"aitp:{challenge.nonce}"
    intent = {
        "payer": payer,
        "payee": challenge.payee,
        "amount_micros": challenge.amount_micros,
        "nonce": challenge.nonce,
        "resource": challenge.resource,
        "expiry": challenge.expiry,
        "scheme": challenge.scheme,
        "idempotency_key": idempotency_key,
    }
    op_hash = blake3_digest(canonical_json(intent))
    signature = signer.sign(op_hash)
    return {
        **intent,
        "pubkey_id": signer.pubkey_id,
        "op_hash": op_hash.hex(),
        "sig": b64url_encode(signature),
    }


def encode_authorization(authorization: Mapping[str, Any]) -> str:
    return b64url_encode(json.dumps(authorization, separators=(",", ":")).encode("utf-8"))


def decode_receipt(header: Optional[str]) -> Optional[Dict[str, Any]]:
    if not header:
        return None
    try:
        return json.loads(b64url_decode(header))
    except ValueError:
        return None


@dataclass
class PaidResponse:
    status: int
    headers: Dict[str, str]
    body: bytes
    paid: bool
    challenge: Optional[Challenge] = None
    authorization: Optional[str] = None
    request_id: Optional[str] = None
    receipt: Optional[Dict[str, Any]] = None
    settlement: Optional[str] = None

    def json(self) -> Any:
        return json.loads(self.body.decode("utf-8"))

    @property
    def text(self) -> str:
        return self.body.decode("utf-8", "replace")

    @property
    def ok(self) -> bool:
        return 200 <= self.status < 300


@dataclass
class McpResult:
    result: Dict[str, Any]
    paid: bool
    receipt: Optional[Dict[str, Any]] = None
    authorization: Optional[str] = None
    request_id: Optional[str] = None


Transport = Callable[[str, str, Dict[str, str], Optional[bytes], float], "tuple[int, Dict[str, str], bytes]"]


def _urllib_transport(
    method: str, url: str, headers: Dict[str, str], body: Optional[bytes], timeout: float
):
    request = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as resp:
            return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, {k.lower(): v for k, v in err.headers.items()}, err.read()
    except urllib.error.URLError as err:
        raise TurnstileError(f"request to {url} failed: {err.reason}") from err


@dataclass
class TurnstileClient:
    """Pays Turnstile-gated tools on behalf of one Mint payer account."""

    base_url: str
    payer: str
    signer: Signer
    policy: SpendPolicy
    timeout: float = 30.0
    transport: Transport = field(default=_urllib_transport, repr=False)
    extra_headers: Dict[str, str] = field(default_factory=dict)

    def __post_init__(self) -> None:
        self.base_url = self.base_url.rstrip("/")
        if not self.payer:
            raise ValueError("payer (Mint account id) is required")

    # ---- discovery ----------------------------------------------------------

    def tools(self) -> Dict[str, Any]:
        """``GET /v1/tools`` — the provider's priced tool catalog (free)."""
        status, _, body = self._send("GET", "/v1/tools", {}, None)
        if status != 200:
            raise HttpError(status, body)
        return json.loads(body)

    def find_tool(self, name: str, kind: str = "http") -> Dict[str, Any]:
        """The catalog entry for ``name`` (``kind`` is ``http`` or ``mcp``)."""
        marker = "tools/call#" if kind == "mcp" else "/v1/"
        for tool in self.tools().get("tools", []):
            if tool.get("name") == name and marker in str(tool.get("resource", "")):
                return tool
        raise TurnstileError(f"tool {name!r} ({kind}) is not in the provider catalog")

    # ---- HTTP -----------------------------------------------------------------

    def request(
        self,
        method: str,
        path: str,
        body: Optional[bytes] = None,
        headers: Optional[Mapping[str, str]] = None,
    ) -> PaidResponse:
        """Send a request, paying one ``402`` challenge if the route is gated."""
        hdrs = {"aitp-version": AITP_VERSION, **self.extra_headers, **(headers or {})}
        status, resp_headers, resp_body = self._send(method, path, hdrs, body)
        if status != 402:
            return PaidResponse(status, resp_headers, resp_body, paid=False)

        encoded = resp_headers.get("aitp-challenge")
        if not encoded:
            raise ProtocolError(
                "402 without AITP-Challenge: " + resp_headers.get("aitp-error", "provider unavailable")
            )
        challenge = Challenge.decode(encoded)
        self.policy.check(challenge)
        authorization = encode_authorization(build_authorization(self.payer, self.signer, challenge))

        hdrs["aitp-authorization"] = authorization
        status, resp_headers, resp_body = self._send(method, path, hdrs, body)
        if status == 402:
            raise PaymentFailed(
                "Turnstile rejected the payment", resp_headers.get("aitp-error")
            )
        receipt = decode_receipt(resp_headers.get("aitp-receipt"))
        if receipt is not None:
            self.policy.record(int(receipt.get("amount_micros", challenge.amount_micros)))
        elif 200 <= status < 300 or resp_headers.get("aitp-settlement") == "capture_pending":
            # Capture pending or not reported: budget the full quote.
            self.policy.record(challenge.amount_micros)
        return PaidResponse(
            status,
            resp_headers,
            resp_body,
            paid=True,
            challenge=challenge,
            authorization=authorization,
            request_id=resp_headers.get("aitp-request-id") or challenge.nonce,
            receipt=receipt,
            settlement=resp_headers.get("aitp-settlement"),
        )

    def post_json(self, path: str, payload: Any) -> PaidResponse:
        return self.request(
            "POST",
            path,
            json.dumps(payload).encode("utf-8"),
            {"content-type": "application/json"},
        )

    def call_tool(self, name: str, arguments: Any) -> PaidResponse:
        """``POST /v1/tools/<name>/call`` with a JSON body, paying on demand."""
        return self.post_json(f"/v1/tools/{urllib.parse.quote(name, safe='')}/call", arguments)

    # ---- settlement -----------------------------------------------------------

    def settlement(self, request_id: str, authorization: str) -> Dict[str, Any]:
        """``GET /v1/settlements/:request_id`` scoped by the paid authorization."""
        status, _, body = self._send(
            "GET",
            f"/v1/settlements/{urllib.parse.quote(request_id, safe='')}",
            {"aitp-authorization": authorization},
            None,
        )
        if status != 200:
            raise HttpError(status, body)
        return json.loads(body)

    def wait_for_settlement(
        self, response: PaidResponse, timeout: float = 60.0, interval: float = 1.0
    ) -> Dict[str, Any]:
        """Poll until the paid request's settlement reaches a terminal phase."""
        if not response.paid or not response.request_id or not response.authorization:
            raise TurnstileError("response was not a paid request")
        deadline = time.monotonic() + timeout
        while True:
            try:
                status = self.settlement(response.request_id, response.authorization)
                if status.get("settled") or status.get("phase") in TERMINAL_PHASES:
                    return status
            except HttpError as err:
                if err.status not in (404, 503):
                    raise
            if time.monotonic() >= deadline:
                raise TurnstileError(
                    f"settlement {response.request_id} not terminal after {timeout}s"
                )
            time.sleep(interval)

    # ---- MCP ------------------------------------------------------------------

    def mcp(self, method: str, params: Optional[Dict[str, Any]] = None, rpc_id: Any = 1) -> Dict[str, Any]:
        """Raw JSON-RPC call to ``POST /mcp`` (returns the whole envelope)."""
        payload = {"jsonrpc": "2.0", "id": rpc_id, "method": method}
        if params is not None:
            payload["params"] = params
        status, _, body = self._send(
            "POST", "/mcp", {"content-type": "application/json"}, json.dumps(payload).encode()
        )
        if status != 200:
            raise HttpError(status, body)
        return json.loads(body)

    def mcp_tools(self) -> List[Dict[str, Any]]:
        envelope = self.mcp("tools/list", {})
        if "error" in envelope:
            raise ProtocolError(f"tools/list failed: {envelope['error']}")
        return envelope.get("result", {}).get("tools", [])

    def mcp_call(self, name: str, arguments: Optional[Dict[str, Any]] = None) -> McpResult:
        """MCP ``tools/call``, paying the AITP challenge when required.

        The challenge arrives as a ``-32402`` JSON-RPC error, or (on an
        x402-enabled server) as an x402 ``PaymentRequired`` tool result that
        carries it in ``_meta["aitp/challenge"]``.
        """
        args = dict(arguments or {})
        envelope = self.mcp("tools/call", {"name": name, "arguments": args})
        first = envelope.get("result")
        x402_challenge = (
            (first.get("_meta") or {}).get("aitp/challenge")
            if isinstance(first, dict) and first.get("isError")
            else None
        )
        if first is not None and not x402_challenge:
            return McpResult(first, paid=False)
        error = (
            {"code": MCP_PAYMENT_REQUIRED, "data": x402_challenge}
            if x402_challenge
            else envelope.get("error") or {}
        )
        if error.get("code") != MCP_PAYMENT_REQUIRED:
            raise TurnstileError(f"tools/call {name} failed: {error.get('message', error)}")
        encoded = (error.get("data") or {}).get("aitp_challenge")
        if not encoded:
            raise ProtocolError(
                "payment required without a challenge: "
                + str((error.get("data") or {}).get("aitp_error", "provider unavailable"))
            )
        challenge = Challenge.decode(encoded)
        self.policy.check(challenge)
        authorization = encode_authorization(build_authorization(self.payer, self.signer, challenge))
        args[AUTHORIZATION_ARG] = authorization
        envelope = self.mcp("tools/call", {"name": name, "arguments": args}, rpc_id=2)
        if "error" in envelope:
            err = envelope["error"]
            if err.get("code") == MCP_PAYMENT_REQUIRED:
                raise PaymentFailed(
                    "Turnstile rejected the payment", (err.get("data") or {}).get("aitp_error")
                )
            raise TurnstileError(f"tools/call {name} failed: {err.get('message', err)}")
        result = envelope.get("result", {})
        meta = result.get("_meta") or {}
        receipt = decode_receipt(meta.get("aitp_receipt"))
        self.policy.record(
            int(meta.get("amount_micros", challenge.amount_micros)) if receipt else challenge.amount_micros
        )
        return McpResult(
            result, paid=True, receipt=receipt, authorization=authorization, request_id=challenge.nonce
        )

    # ---- plumbing -------------------------------------------------------------

    def _send(self, method: str, path: str, headers: Dict[str, str], body: Optional[bytes]):
        return self.transport(method, self.base_url + path, headers, body, self.timeout)
