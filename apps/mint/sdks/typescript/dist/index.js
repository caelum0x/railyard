export const AITP_VERSION = "0.1";
export const HEADER_VERSION = "AITP-Version";
export const HEADER_CHALLENGE = "AITP-Challenge";
export const HEADER_AUTHORIZATION = "AITP-Authorization";
export const HEADER_RECEIPT = "AITP-Receipt";
export class AuthorizationBuilder {
    payer;
    signer;
    constructor(payer, signer) {
        this.payer = payer;
        this.signer = signer;
    }
    async authorize(challenge) {
        validateChallenge(challenge);
        if (challenge.expiry <= Date.now())
            throw new Error("AITP challenge is expired");
        const idempotencyKey = `aitp:${challenge.nonce}`;
        const intent = {
            payer: this.payer,
            payee: challenge.payee,
            amount_micros: challenge.amount_micros,
            nonce: challenge.nonce,
            resource: challenge.resource,
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key: idempotencyKey,
        };
        const sorted = Object.fromEntries(Object.entries(intent).sort(([left], [right]) => left.localeCompare(right)));
        const signed = await this.signer.signAuthorization(new TextEncoder().encode(JSON.stringify(sorted)));
        if (!/^[0-9a-f]{64}$/.test(signed.op_hash)) {
            throw new Error("wallet signer returned a non-canonical BLAKE3 hex digest");
        }
        if (!signed.pubkey_id || !signed.sig) {
            throw new Error("wallet signer returned an empty key id or signature");
        }
        return {
            payer: this.payer,
            payee: challenge.payee,
            amount_micros: challenge.amount_micros,
            nonce: challenge.nonce,
            resource: challenge.resource,
            expiry: challenge.expiry,
            scheme: challenge.scheme,
            idempotency_key: idempotencyKey,
            pubkey_id: signed.pubkey_id,
            op_hash: signed.op_hash,
            sig: signed.sig,
        };
    }
}
export class PaidHttpClient {
    authorizer;
    fetcher;
    constructor(authorizer, fetcher = globalThis.fetch) {
        this.authorizer = authorizer;
        if (!fetcher)
            throw new Error("PaidHttpClient requires a Fetch API implementation");
        this.fetcher = fetcher;
    }
    /** Send once, then sign and retry exactly once for a valid AITP 402. */
    async send(request) {
        const body = await replayableBody(request.body);
        const first = await this.fetcher(request.url, {
            method: request.method,
            ...(request.headers === undefined ? {} : { headers: request.headers }),
            ...(body === undefined ? {} : { body }),
            redirect: "manual",
        });
        if (first.status !== 402)
            return first;
        const encoded = first.headers.get(HEADER_CHALLENGE);
        if (!encoded)
            throw new Error("402 response missing AITP-Challenge header");
        const authorization = await this.authorizer.authorize(decodeChallenge(encoded));
        const headers = new Headers(request.headers);
        headers.set(HEADER_VERSION, AITP_VERSION);
        headers.set(HEADER_AUTHORIZATION, encodeAuthorization(authorization));
        return this.fetcher(request.url, {
            method: request.method,
            headers,
            ...(body === undefined ? {} : { body }),
            redirect: "manual",
        });
    }
}
async function replayableBody(body) {
    if (body === undefined || body === null)
        return undefined;
    if (body instanceof ReadableStream) {
        throw new Error("AITP retry requires a replayable request body, not a ReadableStream");
    }
    if (body instanceof FormData) {
        throw new Error("AITP retry requires stable bytes; pre-encode FormData before sending");
    }
    return body;
}
export function encodeAuthorization(value) {
    return encodeHeader(value);
}
export function decodeAuthorization(header) {
    return decodeHeader(header);
}
export function decodeChallenge(header) {
    const value = decodeHeader(header);
    const challenge = value;
    validateChallenge(challenge);
    return challenge;
}
export function decodeReceipt(header) {
    return decodeHeader(header);
}
function encodeHeader(value) {
    const bytes = new TextEncoder().encode(JSON.stringify(value));
    let binary = "";
    for (const byte of bytes)
        binary += String.fromCharCode(byte);
    return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function decodeHeader(header) {
    const base64 = header.trim().replaceAll("-", "+").replaceAll("_", "/");
    const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
    const binary = atob(padded);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const value = JSON.parse(new TextDecoder().decode(bytes));
    if (!isRecord(value))
        throw new Error("AITP header must encode a JSON object");
    return value;
}
function validateChallenge(value) {
    if (!isRecord(value))
        throw new Error("AITP challenge must be an object");
    if (value.currency !== "AIT")
        throw new Error("AITP currency must be AIT");
    if (value.scheme !== "aitp-hold" && value.scheme !== "aitp-transfer") {
        throw new Error("unsupported AITP scheme");
    }
    if (!Number.isSafeInteger(value.amount_micros) || value.amount_micros < 0) {
        throw new Error("AITP amount_micros must be a non-negative safe integer");
    }
    if (!Number.isSafeInteger(value.expiry))
        throw new Error("AITP expiry must be a safe integer");
    if (!value.payee || !value.resource || !value.nonce) {
        throw new Error("AITP challenge identity fields cannot be empty");
    }
}
export class MintApiError extends Error {
    status;
    code;
    body;
    constructor(status, code, body, message) {
        super(message);
        this.status = status;
        this.code = code;
        this.body = body;
        this.name = "MintApiError";
    }
}
export class MintClient {
    baseUrl;
    apiKey;
    fetcher;
    constructor(options) {
        const url = new URL(options.baseUrl);
        if (url.protocol !== "http:" && url.protocol !== "https:") {
            throw new Error("baseUrl must be an absolute HTTP(S) URL");
        }
        this.baseUrl = options.baseUrl.replace(/\/+$/, "") + "/";
        this.apiKey = options.apiKey;
        this.fetcher = options.fetch ?? globalThis.fetch;
        if (!this.fetcher)
            throw new Error("MintClient requires a Fetch API implementation");
    }
    createAccount(kind, ownerRef) {
        return this.request("POST", "v1/accounts", { kind, owner_ref: ownerRef ?? null });
    }
    account(accountId) {
        return this.request("GET", `v1/accounts/${segment(accountId)}`);
    }
    balance(accountId) {
        return this.request("GET", `v1/accounts/${segment(accountId)}/balance`);
    }
    createAddress(accountId, purpose) {
        return this.request("POST", `v1/accounts/${segment(accountId)}/addresses`, {
            purpose: purpose ?? null,
        });
    }
    lookupAddress(address) {
        return this.request("GET", `v1/addresses/${segment(address)}`);
    }
    createCheckout(provider, accountId, amountMicros) {
        return this.request("POST", "v1/checkouts", {
            provider,
            account_id: accountId,
            amount_micros: amountMicros,
        });
    }
    /**
     * Move integer microdollars exactly once for one logical operation.
     * Keep this account-id API and the Mint service key behind your backend.
     */
    transfer(input) {
        validateTransfer(input);
        if (input.to.startsWith("ait1")) {
            throw new Error("public Mint addresses must use transferToAddress so the signed receipt can be bound to the resolved recipient");
        }
        return this.request("POST", "v1/transfers", input).then((value) => {
            validateTransferReceipt(value, input);
            return value;
        });
    }
    /** Resolve a public receive address, then bind the signed proof to its account. */
    async transferToAddress(input) {
        if (!input.address.startsWith("ait1")) {
            throw new Error("public Mint addresses must begin with ait1");
        }
        const recipient = await this.lookupAddress(input.address);
        if (typeof recipient.account_id !== "string" || recipient.account_id.length === 0) {
            throw new MintApiError(200, "invalid_response", recipient, "Mint address lookup is invalid");
        }
        const { address: _address, ...transfer } = input;
        return this.transfer({ ...transfer, to: recipient.account_id });
    }
    createHold(input) {
        return this.request("POST", "v1/holds", input);
    }
    hold(holdId) {
        return this.request("GET", `v1/holds/${segment(holdId)}`);
    }
    captureHold(holdId, input) {
        return this.request("POST", `v1/holds/${segment(holdId)}/capture`, input);
    }
    releaseHold(holdId, input) {
        return this.request("POST", `v1/holds/${segment(holdId)}/release`, input);
    }
    grantAllowance(input) {
        return this.request("POST", "v1/allowances", input);
    }
    allowance(allowanceId) {
        return this.request("GET", `v1/allowances/${segment(allowanceId)}`);
    }
    spendAllowance(allowanceId, input) {
        return this.request("POST", `v1/allowances/${segment(allowanceId)}/spend`, input);
    }
    revokeAllowance(allowanceId, idemKey) {
        return this.request("POST", `v1/allowances/${segment(allowanceId)}/revoke`, {
            idem_key: idemKey,
        });
    }
    openStream(input) {
        validatePositiveMoney("stream rate_micros", input.rate_micros);
        validatePositiveMoney("stream cap_micros", input.cap_micros);
        return this.request("POST", "v1/streams", input);
    }
    stream(streamId) {
        return this.request("GET", `v1/streams/${segment(streamId)}`);
    }
    tickStream(streamId, input) {
        validateCounter("stream units", input.units);
        validateCounter("stream seq", input.seq);
        return this.request("POST", `v1/streams/${segment(streamId)}/tick`, input);
    }
    closeStream(streamId, input) {
        validateCounter("stream units", input.units);
        validateCounter("stream seq", input.seq);
        return this.request("POST", `v1/streams/${segment(streamId)}/close`, input);
    }
    accountReceipts(accountId) {
        return this.request("GET", `v1/accounts/${segment(accountId)}/receipts`);
    }
    receiptAudit() {
        return this.request("GET", "v1/receipts/audit");
    }
    reserve() {
        return this.request("GET", "v1/reserve");
    }
    mint(input) {
        validatePositiveMoney("mint amount_micros", input.amount_micros);
        return this.request("POST", "v1/mint", input);
    }
    redeem(input) {
        validatePositiveMoney("redeem amount_micros", input.amount_micros);
        return this.request("POST", "v1/redeem", input);
    }
    verificationKeys() {
        return this.request("GET", ".well-known/mint-verification-keys.json");
    }
    async request(method, path, body) {
        const headers = new Headers({ Accept: "application/json", "X-API-Key": this.apiKey });
        if (body !== undefined)
            headers.set("Content-Type", "application/json");
        const response = await this.fetcher(new URL(path, this.baseUrl), {
            method,
            headers,
            ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        });
        const payload = await response.json().catch(() => null);
        const data = isRecord(payload) ? payload.data : undefined;
        if (!response.ok
            || !isRecord(payload)
            || payload.success !== true
            || (!isRecord(data) && !Array.isArray(data))) {
            const detail = isRecord(payload) && isRecord(payload.error) ? payload.error : null;
            const code = detail && typeof detail.code === "string" ? detail.code : "http_error";
            const message = detail && typeof detail.message === "string"
                ? detail.message
                : `Mint request failed with HTTP ${response.status}`;
            throw new MintApiError(response.status, code, payload, message);
        }
        return data;
    }
}
function isRecord(value) {
    return typeof value === "object" && value !== null && !Array.isArray(value);
}
function segment(value) {
    return encodeURIComponent(value);
}
function validateTransfer(input) {
    for (const [name, value] of [
        ["from", input.from],
        ["to", input.to],
        ["idem_key", input.idem_key],
    ]) {
        if (typeof value !== "string" || value.trim().length === 0) {
            throw new Error(`transfer ${name} must be a non-empty string`);
        }
    }
    if (!Number.isSafeInteger(input.amount_micros) || input.amount_micros <= 0) {
        throw new Error("transfer amount_micros must be a positive safe integer");
    }
    if (input.idem_key.length > 128) {
        throw new Error("transfer idem_key must be at most 128 characters");
    }
    if (input.ref !== undefined && typeof input.ref !== "string") {
        throw new Error("transfer ref must be a string when provided");
    }
}
function validatePositiveMoney(name, value) {
    if (!Number.isSafeInteger(value) || value <= 0) {
        throw new Error(`${name} must be a positive safe integer`);
    }
}
function validateCounter(name, value) {
    if (!Number.isSafeInteger(value) || value < 0) {
        throw new Error(`${name} must be a non-negative safe integer`);
    }
}
function validateTransferReceipt(value, input) {
    const receipt = value.receipt;
    const attestation = value.attestation;
    if (!Number.isSafeInteger(value.first_seq) || value.first_seq < 0
        || !isRecord(value.from_balance)
        || !isRecord(value.to_balance)
        || typeof value.idempotent_replay !== "boolean"
        || !isRecord(receipt)
        || !isRecord(attestation)
        || !isRecord(receipt.body)) {
        throw new MintApiError(200, "invalid_response", value, "Mint transfer proof is incomplete");
    }
    const body = receipt.body;
    const requiredStrings = [
        receipt.hash,
        receipt.sig,
        body.op,
        body.prev_hash,
        body.signer,
        attestation.payload_base64url,
        attestation.digest_sha256,
        attestation.signer_key_id,
        attestation.signature_hex,
    ];
    if (requiredStrings.some((item) => typeof item !== "string" || item.length === 0)) {
        throw new MintApiError(200, "invalid_response", value, "Mint transfer proof fields are invalid");
    }
    if (attestation.format !== "mint-transfer-attestation-v1") {
        throw new MintApiError(200, "invalid_response", value, "Mint transfer attestation format is invalid");
    }
    if (body.account_id !== input.from
        || body.from !== input.from
        || body.to !== input.to
        || body.journal_seq !== value.first_seq
        || body.amount_micros !== input.amount_micros
        || (body.op !== "Transfer" && body.op !== "transfer")) {
        throw new MintApiError(200, "invalid_response", value, "Mint transfer proof does not match request");
    }
}
