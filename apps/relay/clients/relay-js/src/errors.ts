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
): RelayApiError {
  const parsed = extractErrorBody(body);
  const message =
    parsed.message ?? `Relay request failed with status ${status}`;

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
