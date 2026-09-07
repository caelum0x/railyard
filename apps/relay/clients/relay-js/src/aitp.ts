import type { AitpChallenge, PaymentRequired } from "./types.js";

export const AITP_CHALLENGE_HEADER = "aitp-challenge";
export const AITP_AUTHORIZATION_HEADER = "aitp-authorization";
export const AITP_REQUEST_ID_HEADER = "aitp-request-id";
export const AITP_VERSION_HEADER = "aitp-version";
export const AITP_RECEIPT_HEADER = "aitp-receipt";

export interface HeaderReader {
  get(name: string): string | null;
}

export function decodeAitpChallenge(encoded: string): AitpChallenge {
  let decoded: unknown;
  try {
    const normalized = encoded.replace(/-/g, "+").replace(/_/g, "/");
    const padded = normalized.padEnd(Math.ceil(normalized.length / 4) * 4, "=");
    const binary = atob(padded);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    decoded = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new Error("Relay returned an invalid AITP challenge encoding");
  }
  if (!isAitpChallenge(decoded)) {
    throw new Error("Relay returned invalid AITP challenge fields");
  }
  return decoded;
}

export function paymentRequiredFromHeaders(headers: HeaderReader): PaymentRequired {
  const encoded = headers.get(AITP_CHALLENGE_HEADER);
  if (!encoded) {
    throw new Error("Relay returned HTTP 402 without an AITP challenge header");
  }
  const requestId = headers.get(AITP_REQUEST_ID_HEADER) ?? undefined;
  if (requestId && !isUuid(requestId)) {
    throw new Error("Relay returned an invalid AITP request id");
  }
  return {
    challenge: decodeAitpChallenge(encoded),
    request_id: requestId,
    protocol_version: headers.get(AITP_VERSION_HEADER) ?? undefined,
  };
}

function isAitpChallenge(value: unknown): value is AitpChallenge {
  if (!value || typeof value !== "object") return false;
  const challenge = value as Partial<AitpChallenge>;
  return (
    typeof challenge.payee === "string" && challenge.payee.length > 0 &&
    typeof challenge.amount_micros === "number" &&
    Number.isSafeInteger(challenge.amount_micros) && challenge.amount_micros >= 0 &&
    challenge.currency === "AIT" &&
    typeof challenge.resource === "string" && challenge.resource.length > 0 &&
    typeof challenge.nonce === "string" && challenge.nonce.length > 0 &&
    typeof challenge.expiry === "number" && Number.isSafeInteger(challenge.expiry) &&
    challenge.expiry >= 0 && challenge.scheme === "aitp-hold"
  );
}

function isUuid(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);
}
