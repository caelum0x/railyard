export class VendApiError extends Error {
    status;
    code;
    body;
    settlement;
    constructor(status, body, settlement) {
        const error = isRecord(body) && isRecord(body.error) ? body.error : null;
        const message = error && typeof error.message === "string"
            ? error.message
            : `Vend request failed with HTTP ${status}`;
        super(message);
        this.name = "VendApiError";
        this.status = status;
        this.code = error && typeof error.code === "string" ? error.code : null;
        this.body = body;
        this.settlement = settlement;
    }
}
export class VendChatStream {
    status;
    settlement;
    response;
    consumed = false;
    constructor(response) {
        this.response = response;
        this.status = response.status;
        this.settlement = settlementHeaders(response.headers);
    }
    async *[Symbol.asyncIterator]() {
        if (this.consumed)
            throw new Error("a Vend stream can only be consumed once");
        this.consumed = true;
        const reader = this.response.body?.getReader();
        if (!reader)
            throw new Error("Vend returned a streaming response without a body");
        const decoder = new TextDecoder();
        let buffer = "";
        let finished = false;
        try {
            while (true) {
                const chunk = await reader.read();
                if (chunk.done) {
                    finished = true;
                    buffer += decoder.decode();
                    break;
                }
                buffer += decoder.decode(chunk.value, { stream: true });
                const frames = splitFrames(buffer);
                buffer = frames.remainder;
                for (const frame of frames.complete)
                    yield parseEvent(frame);
            }
            if (buffer.trim() !== "")
                yield parseEvent(buffer);
        }
        finally {
            if (!finished)
                await reader.cancel().catch(() => undefined);
            reader.releaseLock();
        }
    }
}
export class VendClient {
    baseUrl;
    customerSession;
    provisioningKey;
    fetcher;
    constructor(options) {
        this.baseUrl = normalizeBaseUrl(options.baseUrl);
        this.customerSession = options.customerSession;
        this.provisioningKey = options.provisioningKey;
        this.fetcher = options.fetch ?? globalThis.fetch;
        if (!this.fetcher)
            throw new Error("VendClient requires a Fetch API implementation");
    }
    setCustomerSession(session) {
        this.customerSession = session;
    }
    setProvisioningKey(key) {
        this.provisioningKey = key;
    }
    oauthStartUrl(provider, tenantId) {
        const url = new URL(resolveUrl(`/v1/auth/${provider}/start`, this.baseUrl));
        url.searchParams.set("tenant_id", tenantId);
        return url.toString();
    }
    async setup(setupToken, password) {
        return this.control("/v1/auth/setup", "POST", {
            setup_token: setupToken,
            password,
        }, false);
    }
    async login(tenantId, email, password) {
        const session = await this.control("/v1/auth/login", "POST", {
            tenant_id: tenantId,
            email,
            password,
        }, false);
        this.customerSession = session.session_token;
        return session;
    }
    async logout() {
        const result = await this.control("/v1/customer/logout", "POST");
        this.customerSession = undefined;
        return result;
    }
    tenant() {
        return this.control("/v1/customer/tenant", "GET");
    }
    listApps() {
        return this.control("/v1/customer/apps", "GET");
    }
    getApp(appId) {
        return this.control(`/v1/customer/apps/${segment(appId)}`, "GET");
    }
    createFundingCheckout(appId, provider, amountMicros) {
        return this.control(`/v1/customer/apps/${segment(appId)}/funding-checkout`, "POST", { provider, amount_micros: amountMicros });
    }
    createApp(input) {
        return this.control("/v1/customer/apps", "POST", input);
    }
    setAppEnabled(appId, enabled) {
        return this.control(`/v1/customer/apps/${segment(appId)}/enabled`, "PATCH", { enabled });
    }
    listKeys(appId) {
        return this.control(`/v1/customer/apps/${segment(appId)}/keys`, "GET");
    }
    issueKey(appId, input) {
        return this.control(`/v1/customer/apps/${segment(appId)}/keys`, "POST", {
            environment: input.environment ?? "live",
            daily_cap_micros: input.daily_cap_micros,
            per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros ?? null,
        });
    }
    revokeKey(keyId) {
        return this.control(`/v1/customer/keys/${segment(keyId)}`, "DELETE");
    }
    usage(appId, utcDay) {
        const path = new URL(resolveUrl(`/v1/customer/apps/${segment(appId)}/usage`, this.baseUrl));
        if (utcDay)
            path.searchParams.set("utc_day", utcDay);
        return this.control(path.toString(), "GET");
    }
    async chatCompletions(body, idempotencyKey, endUser) {
        const response = await this.chatRequest({ ...body, stream: false }, idempotencyKey, endUser);
        const payload = await response.json().catch(() => null);
        const settlement = settlementHeaders(response.headers);
        if (!response.ok)
            throw new VendApiError(response.status, payload, settlement);
        return { body: payload, status: response.status, settlement };
    }
    async chatCompletionsStream(body, idempotencyKey, endUser) {
        const response = await this.chatRequest({ ...body, stream: true }, idempotencyKey, endUser);
        if (!response.ok) {
            const payload = await response.json().catch(() => null);
            throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
        }
        return new VendChatStream(response);
    }
    async control(path, method, body, authenticated = true) {
        const headers = new Headers({ Accept: "application/json" });
        if (body !== undefined)
            headers.set("Content-Type", "application/json");
        if (authenticated) {
            if (!this.customerSession)
                throw new Error("customerSession is required for this operation");
            headers.set("Authorization", `Bearer ${this.customerSession}`);
        }
        const response = await this.fetcher(resolveUrl(path, this.baseUrl), {
            method,
            headers,
            ...(body === undefined ? {} : { body: JSON.stringify(body) }),
            redirect: "manual",
        });
        const payload = await response.json().catch(() => null);
        if (!response.ok || !isRecord(payload) || payload.success !== true || !("data" in payload)) {
            throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
        }
        return payload.data;
    }
    chatRequest(body, idempotencyKey, endUser) {
        if (!this.provisioningKey)
            throw new Error("provisioningKey is required for chat completions");
        if (!idempotencyKey)
            throw new Error("idempotencyKey is required for chat completions");
        const headers = new Headers({
            Accept: body.stream === true ? "text/event-stream" : "application/json",
            Authorization: `Bearer ${this.provisioningKey}`,
            "Content-Type": "application/json",
            "Idempotency-Key": idempotencyKey,
        });
        if (endUser)
            headers.set("Vend-End-User", endUser);
        return this.fetcher(resolveUrl("/v1/chat/completions", this.baseUrl), {
            method: "POST",
            headers,
            body: JSON.stringify(body),
            redirect: "manual",
        });
    }
}
function normalizeBaseUrl(value) {
    const url = new URL(value);
    url.search = "";
    url.hash = "";
    return url.toString();
}
function resolveUrl(path, baseUrl) {
    try {
        return new URL(path).toString();
    }
    catch {
        return new URL(path.replace(/^\/+/, ""), `${baseUrl.replace(/\/+$/, "")}/`).toString();
    }
}
function segment(value) {
    return encodeURIComponent(value);
}
function isRecord(value) {
    return typeof value === "object" && value !== null;
}
function parseIntegerHeader(headers, name) {
    const raw = headers.get(name);
    if (raw === null || !/^-?\d+$/.test(raw))
        return null;
    const value = Number(raw);
    return Number.isSafeInteger(value) ? value : null;
}
function settlementHeaders(headers) {
    return {
        chargeId: headers.get("Vend-Charge-Id"),
        baseCostMicros: parseIntegerHeader(headers, "Vend-Base-Cost-Micros"),
        priceMicros: parseIntegerHeader(headers, "Vend-Price-Micros"),
        model: headers.get("Vend-Model"),
        requestId: headers.get("X-Request-Id"),
    };
}
function splitFrames(buffer) {
    const normalized = buffer.replace(/\r\n/g, "\n");
    const parts = normalized.split("\n\n");
    return { complete: parts.slice(0, -1), remainder: parts.at(-1) ?? "" };
}
function parseEvent(frame) {
    let event = null;
    let id = null;
    let retry = null;
    const data = [];
    for (const line of frame.split("\n")) {
        if (line === "" || line.startsWith(":"))
            continue;
        const colon = line.indexOf(":");
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? "" : line.slice(colon + 1);
        if (value.startsWith(" "))
            value = value.slice(1);
        if (field === "data")
            data.push(value);
        else if (field === "event")
            event = value;
        else if (field === "id")
            id = value;
        else if (field === "retry" && /^\d+$/.test(value))
            retry = Number(value);
    }
    return { event, data: data.join("\n"), id, retry };
}
