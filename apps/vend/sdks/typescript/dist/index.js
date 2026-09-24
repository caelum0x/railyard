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
    /** Inclusive per-day usage series (max 92 days). */
    usageRange(appId, from, to) {
        const path = new URL(resolveUrl(`/v1/customer/apps/${segment(appId)}/usage`, this.baseUrl));
        path.searchParams.set("from", from);
        path.searchParams.set("to", to);
        return this.control(path.toString(), "GET");
    }
    /** Live Mint balance of the app's reserve wallet. */
    balance(appId) {
        return this.control(`/v1/customer/apps/${segment(appId)}/balance`, "GET");
    }
    provisionEndUser(appId, externalRef, mintAccountId) {
        return this.control(`/v1/customer/apps/${segment(appId)}/end-users`, "POST", {
            external_ref: externalRef,
            mint_account_id: mintAccountId,
        });
    }
    listUsers() {
        return this.control("/v1/customer/users", "GET");
    }
    inviteUser(email, displayName, role) {
        return this.control("/v1/customer/users", "POST", {
            email,
            display_name: displayName,
            role,
        });
    }
    updateUser(userId, changes) {
        return this.control(`/v1/customer/users/${segment(userId)}`, "PATCH", changes);
    }
    revokeUserSessions(userId) {
        return this.control(`/v1/customer/users/${segment(userId)}/sessions`, "DELETE");
    }
    changePassword(currentPassword, newPassword) {
        return this.control("/v1/customer/password", "PATCH", {
            current_password: currentPassword,
            new_password: newPassword,
        });
    }
    /** Rotate the current session; the client switches to the new token. */
    /** Self-serve workspace signup (when the operator enabled it). Signs in. */
    async signup(input) {
        const result = await this.control("/v1/auth/signup", "POST", input, false);
        this.customerSession = result.session_token;
        return result;
    }
    // ---- End-user wallets from your backend (provisioning key) ------------------
    /** Create (idempotently) the wallet of one of your users. */
    createWallet(externalRef) {
        return this.keyed("/v1/wallets", "POST", { external_ref: externalRef });
    }
    /** Live wallet balance and today's spend. */
    getWallet(externalRef) {
        return this.keyed(`/v1/wallets/${segment(externalRef)}`, "GET");
    }
    /** Hosted checkout that deposits into the user's wallet. Whole cents only. */
    createWalletTopUp(externalRef, provider, amountMicros) {
        return this.keyed(`/v1/wallets/${segment(externalRef)}/top-ups`, "POST", {
            provider,
            amount_micros: amountMicros,
        });
    }
    // ---- White-label console (customer session) -----------------------------------
    listWallets(appId, limit = 50, offset = 0) {
        return this.control(`/v1/customer/apps/${segment(appId)}/wallets?limit=${limit}&offset=${offset}`, "GET");
    }
    getModelPricing(appId) {
        return this.control(`/v1/customer/apps/${segment(appId)}/model-pricing`, "GET");
    }
    setModelMarkup(appId, model, markup) {
        return this.control(`/v1/customer/apps/${segment(appId)}/model-pricing`, "PUT", {
            model,
            ...markup,
        });
    }
    clearModelMarkup(appId, model) {
        return this.control(`/v1/customer/apps/${segment(appId)}/model-pricing?model=${encodeURIComponent(model)}`, "DELETE");
    }
    earnings(from, to) {
        const query = new URLSearchParams();
        if (from)
            query.set("from", from);
        if (to)
            query.set("to", to);
        return this.control(`/v1/customer/earnings?${query.toString()}`, "GET");
    }
    async rotateSession() {
        const session = await this.control("/v1/customer/session/rotate", "POST");
        this.customerSession = session.session_token;
        return session;
    }
    /** OpenAI-compatible model list for the provisioning key, with effective rates. */
    async listModels() {
        if (!this.provisioningKey)
            throw new Error("provisioningKey is required to list models");
        const response = await this.fetcher(resolveUrl("/v1/models", this.baseUrl), {
            method: "GET",
            headers: { Accept: "application/json", Authorization: `Bearer ${this.provisioningKey}` },
        });
        const payload = await response.json().catch(() => null);
        if (!response.ok || !isRecord(payload) || !Array.isArray(payload.data)) {
            throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
        }
        return payload.data;
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
    /** `{success,data}` call authenticated with the provisioning key. */
    async keyed(path, method, body) {
        if (!this.provisioningKey)
            throw new Error("provisioningKey is required for wallet operations");
        const headers = new Headers({
            Accept: "application/json",
            Authorization: `Bearer ${this.provisioningKey}`,
        });
        if (body !== undefined)
            headers.set("Content-Type", "application/json");
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
