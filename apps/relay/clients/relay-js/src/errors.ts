/**
 * Error hierarchy for the Relay SDK.
 *
 * Non-2xx responses from the gateway carry a JSON envelope shaped like
 * `{ error: { message, type, code } }`. We normalize known cases into typed
 * subclasses so callers can branch on `instanceof` instead of parsing bodies.
 */

export interface RelayErrorBody {
  message?: string;
  type?: string;
  code?: string;
}

export class RelayApiError extends Error {
  readonly status: number;
  readonly type?: string;
  readonly code?: string;
  readonly body: unknown;

  constructor(
    message: string,
    options: { status: number; type?: string; code?: string; body?: unknown },
  ) {
    super(message);
    this.name = "RelayApiError";
    this.status = options.status;
    this.type = options.type;
    this.code = options.code;
    this.body = options.body;
    // Preserve prototype chain when targeting older transpile targets.
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** 402 — the billing middleware rejected the request for lack of credits. */
export class InsufficientCreditsError extends RelayApiError {
  constructor(
    message = "insufficient credits",
    options: { status?: number; type?: string; code?: string; body?: unknown } = {},
  ) {
    super(message, {
      status: options.status ?? 402,
      type: options.type ?? "insufficient_credits",
      code: options.code ?? "insufficient_credits",
      body: options.body,
    });
    this.name = "InsufficientCreditsError";
  }
}

/** 402 carrying Relay's typed AITP hold challenge. */
export class PaymentRequiredError extends RelayApiError {
  readonly payment: PaymentRequired;

  constructor(payment: PaymentRequired, body?: unknown) {
    super("Relay requires AITP payment", {
      status: 402,
      type: "payment_required",
      code: "aitp_payment_required",
      body,
    });
    this.name = "PaymentRequiredError";
    this.payment = payment;
  }
}

/** 409 — the paid request already captured and carries terminal evidence. */
export class AlreadySettledError extends RelayApiError {
  readonly evidence: SettlementEvidence;

  constructor(evidence: SettlementEvidence, body?: unknown) {
    super("Relay request already settled", {
      status: 409,
      type: "request_already_settled",
      code: "request_already_settled",
      body,
    });
    this.name = "AlreadySettledError";
    this.evidence = evidence;
  }
}

/** A replay projection that has not been independently verified against Mint. */
export class UnverifiedSettlementEvidenceError extends RelayApiError {
  readonly evidence: SettlementEvidence;

  constructor(evidence: SettlementEvidence, body?: unknown) {
    super("Relay returned settlement evidence but no pinned Mint verifier is configured", {
      status: 409,
      type: "unverified_settlement_evidence",
      code: "unverified_settlement_evidence",
      body,
    });
    this.name = "UnverifiedSettlementEvidenceError";
    this.evidence = evidence;
  }
}

export class InvalidSettlementEvidenceError extends RelayApiError {
  readonly evidence: SettlementEvidence;

  constructor(evidence: SettlementEvidence, body?: unknown) {
    super("Relay settlement evidence failed independent Mint verification", {
      status: 409,
      type: "invalid_settlement_evidence",
      code: "invalid_settlement_evidence",
      body,
    });
    this.name = "InvalidSettlementEvidenceError";
    this.evidence = evidence;
  }
}

/** 401 / 403 — missing, invalid, or unauthorized bearer key. */
export class AuthError extends RelayApiError {
  constructor(
    message: string,
    options: { status: number; type?: string; code?: string; body?: unknown },
  ) {
    super(message, options);
    this.name = "AuthError";
  }
}

/**
 * Build the most specific error subclass for a failed HTTP response.
 */
export function errorFromResponse(
  status: number,
  body: unknown,
  headers?: HeaderReader,
): RelayApiError {
  const parsed = extractErrorBody(body);
  const message =
    parsed.message ?? `Relay request failed with status ${status}`;

  if (status === 402 && headers?.get("aitp-challenge")) {
    try {
      return new PaymentRequiredError(paymentRequiredFromHeaders(headers), body);
    } catch (cause) {
      return new RelayApiError(
        cause instanceof Error ? cause.message : "Relay returned an invalid AITP challenge",
        {
          status,
          type: "invalid_payment_challenge",
          code: "invalid_payment_challenge",
          body,
        },
      );
    }
  }

  if (status === 402 || parsed.type === "insufficient_credits") {
    return new InsufficientCreditsError(message, {
      status,
      type: parsed.type,
      code: parsed.code,
      body,
    });
  }

  if (status === 401 || status === 403) {
    return new AuthError(message, {
      status,
      type: parsed.type,
      code: parsed.code,
      body,
    });
  }

  if (
    status === 409 &&
    parsed.code === "request_already_settled"
  ) {
    const evidence = extractSettlementEvidence(body);
    if (evidence) return new UnverifiedSettlementEvidenceError(evidence, body);
  }

  return new RelayApiError(message, {
    status,
    type: parsed.type,
    code: parsed.code,
    body,
  });
}

function extractErrorBody(body: unknown): RelayErrorBody {
  if (body && typeof body === "object" && "error" in body) {
    const err = (body as { error: unknown }).error;
    if (err && typeof err === "object") {
      return err as RelayErrorBody;
    }
    if (typeof err === "string") {
      return { message: err };
    }
  }
  if (typeof body === "string" && body.length > 0) {
    return { message: body };
  }
  return {};
}

function extractSettlementEvidence(body: unknown): SettlementEvidence | undefined {
  if (!body || typeof body !== "object" || !("data" in body)) return undefined;
  const evidence = (body as { data: unknown }).data;
  if (!evidence || typeof evidence !== "object") return undefined;
  const value = evidence as Partial<SettlementEvidence>;
  const receipt = value.receipt as Partial<SettlementEvidence["receipt"]> | undefined;
  if (
    typeof value.request_id !== "string" ||
    typeof value.principal !== "string" ||
    typeof value.settled_at_ms !== "number" ||
    !Number.isSafeInteger(value.settled_at_ms) ||
    !receipt ||
    typeof receipt.sequence !== "number" ||
    !Number.isSafeInteger(receipt.sequence) ||
    typeof receipt.operation !== "string" ||
    typeof receipt.operation_hash !== "string" ||
    typeof receipt.amount_micros !== "number" ||
    !Number.isSafeInteger(receipt.amount_micros) ||
    typeof receipt.receipt_hash !== "string" ||
    typeof receipt.signature !== "string"
  ) {
    return undefined;
  }
  return value as SettlementEvidence;
}
import { paymentRequiredFromHeaders, type HeaderReader } from "./aitp.js";
import type { PaymentRequired, SettlementEvidence } from "./types.js";
