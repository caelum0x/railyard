import { describe, expect, it } from "vitest";
import {
  AlreadySettledError,
  MultipartForm,
  PaymentRequiredError,
  RelayClient,
  UnverifiedSettlementEvidenceError,
  type AitpChallenge,
  type FetchLike,
  type FetchLikeResponse,
} from "../src/index.js";

const BASE = "https://relay.test/root";
const REQUEST_ID = "018f7e23-7b55-7d9a-8000-000000000002";

interface Call {
  url: string;
  init?: Parameters<FetchLike>[1];
}

function response(
  status: number,
  body: unknown,
  headers: Record<string, string> = {},
): FetchLikeResponse {
  const text = typeof body === "string" ? body : JSON.stringify(body);
  const normalized = new Map(
    Object.entries(headers).map(([key, value]) => [key.toLowerCase(), value]),
  );
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (name) => normalized.get(name.toLowerCase()) ?? null },
    json: async () => JSON.parse(text),
    text: async () => text,
    body: null,
  };
}

function streamResponse(
  frames: string,
  requestId = REQUEST_ID,
): FetchLikeResponse {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(frames));
      controller.close();
    },
  });
  return {
    ...response(200, "", {
      "content-type": "text/event-stream",
      "aitp-request-id": requestId,
    }),
    body: stream,
  };
}

function queueFetch(responses: FetchLikeResponse[]): { fetch: FetchLike; calls: Call[] } {
  const calls: Call[] = [];
  const fetch: FetchLike = async (url, init) => {
    calls.push({ url, init });
    const next = responses.shift();
    if (!next) throw new Error("no queued response");
    return next;
  };
  return { fetch, calls };
}

function challenge(): AitpChallenge {
  return {
    payee: "ait1qrelay",
    amount_micros: 7_500,
    currency: "AIT",
    resource: `POST /openai/v1/chat/completions#openai/gpt-4o@${REQUEST_ID}`,
    nonce: "nonce-1",
    expiry: 1_800_000_000_000,
    scheme: "aitp-hold",
  };
}

function encodedChallenge(): string {
  return Buffer.from(JSON.stringify(challenge())).toString("base64url");
}

function chatPayload() {
  return {
    id: "chatcmpl-1",
    object: "chat.completion",
    created: 1,
    model: "gpt-4o",
    choices: [{ index: 0, finish_reason: "stop", message: { role: "assistant", content: "ok" } }],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
  };
}

function params() {
  return {
    model: "tensorzero::model_name::openai::gpt-4o",
    messages: [{ role: "user" as const, content: "hello" }],
  };
}

describe("AITP paid journeys", () => {
  it("decodes the challenge and retries exactly once with bound headers", async () => {
    const { fetch, calls } = queueFetch([
      response(402, { error: { message: "payment required" } }, {
        "aitp-challenge": encodedChallenge(),
        "aitp-request-id": REQUEST_ID,
        "aitp-version": "1",
      }),
      response(200, chatPayload(), { "content-type": "application/json" }),
    ]);
    const client = new RelayClient({ baseUrl: BASE, apiKey: "key", fetch });
    const result = await client.paidChatCompletions(params(), {
      authorize(payment) {
        expect(payment.challenge).toEqual(challenge());
        expect(payment.request_id).toBe(REQUEST_ID);
        return "signed-authorization";
      },
    });

    expect(result.choices[0].message.content).toBe("ok");
    expect(calls).toHaveLength(2);
    expect(calls[0].url).toBe(`${BASE}/openai/v1/chat/completions`);
    expect(calls[1].init?.headers?.["aitp-authorization"]).toBe("signed-authorization");
    expect(calls[1].init?.headers?.["aitp-request-id"]).toBe(REQUEST_ID);
  });

  it("returns a second challenge instead of retrying forever", async () => {
    const payment = () => response(402, {}, {
      "aitp-challenge": encodedChallenge(),
      "aitp-request-id": REQUEST_ID,
    });
    const { fetch, calls } = queueFetch([payment(), payment()]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    await expect(
      client.paidChatCompletions(params(), { authorize: () => "signed" }),
    ).rejects.toBeInstanceOf(PaymentRequiredError);
    expect(calls).toHaveLength(2);
  });

  it("never trusts settled evidence without an independent verifier", async () => {
    const evidence = {
      request_id: REQUEST_ID,
      principal: "acct_owner",
      receipt: {
        sequence: 7,
        operation: "hold.capture",
        operation_hash: "op",
        amount_micros: 42,
        receipt_hash: "hash",
        signature: "sig",
      },
      settled_at_ms: 123,
    };
    const { fetch } = queueFetch([
      response(409, {
        error: {
          message: "request already settled",
          type: "request_already_settled",
          code: "request_already_settled",
        },
        data: evidence,
      }),
    ]);
    await expect(
      new RelayClient({ baseUrl: BASE, fetch }).inference({ model_name: "m" }),
    ).rejects.toBeInstanceOf(UnverifiedSettlementEvidenceError);
  });

  it("requires verification for direct settlement lookup too", async () => {
    const evidence = {
      request_id: REQUEST_ID,
      principal: "acct_owner",
      receipt: {
        sequence: 7,
        operation: "hold.capture",
        operation_hash: "op",
        amount_micros: 42,
        receipt_hash: "hash",
        signature: "sig",
      },
      settled_at_ms: 123,
    };
    const first = queueFetch([response(200, { data: evidence })]);
    await expect(
      new RelayClient({ baseUrl: BASE, fetch: first.fetch }).settlementEvidence(REQUEST_ID),
    ).rejects.toBeInstanceOf(UnverifiedSettlementEvidenceError);

    const second = queueFetch([response(200, { data: evidence })]);
    const verified = await new RelayClient({
      baseUrl: BASE,
      fetch: second.fetch,
      settlementEvidenceVerifier: (candidate) => candidate.request_id === REQUEST_ID,
    }).settlementEvidence(REQUEST_ID);
    expect(verified).toEqual(evidence);
  });

  it("returns typed evidence when the paid retry was already settled", async () => {
    const payment = response(402, {}, {
      "aitp-challenge": encodedChallenge(),
      "aitp-request-id": REQUEST_ID,
    });
    const evidence = {
      request_id: REQUEST_ID,
      principal: "acct_owner",
      receipt: {
        sequence: 7,
        operation: "hold.capture",
        operation_hash: "op",
        amount_micros: 42,
        receipt_hash: "hash",
        signature: "sig",
      },
      settled_at_ms: 123,
    };
    const { fetch, calls } = queueFetch([
      payment,
      response(409, {
        error: {
          message: "request already settled",
          type: "request_already_settled",
          code: "request_already_settled",
        },
        data: evidence,
      }),
    ]);
    const client = new RelayClient({
      baseUrl: BASE,
      fetch,
      settlementEvidenceVerifier: async (candidate) =>
        candidate.request_id === evidence.request_id &&
        candidate.receipt.receipt_hash === evidence.receipt.receipt_hash,
    });

    let observed: unknown;
    try {
      await client.paidChatCompletions(params(), { authorize: () => "signed" });
    } catch (error) {
      observed = error;
    }
    expect(observed).toBeInstanceOf(AlreadySettledError);
    expect((observed as AlreadySettledError).evidence).toEqual(evidence);
    expect(calls).toHaveLength(2);
  });

  it("authorizes before exposing a paid SSE stream", async () => {
    const payment = response(402, {}, {
      "aitp-challenge": encodedChallenge(),
      "aitp-request-id": REQUEST_ID,
    });
    const frame = `data: ${JSON.stringify({
      id: "chatcmpl-1",
      object: "chat.completion.chunk",
      created: 1,
      model: "gpt-4o",
      choices: [{ index: 0, finish_reason: null, delta: { content: "paid" } }],
    })}\n\ndata: [DONE]\n\n`;
    const { fetch, calls } = queueFetch([payment, streamResponse(frame)]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    const chunks = [];
    for await (const chunk of client.paidChatCompletionsStream(params(), {
      authorize: () => "stream-authorization",
    })) {
      chunks.push(chunk);
    }
    expect(chunks[0].choices[0].delta.content).toBe("paid");
    expect(calls).toHaveLength(2);
    expect(calls[0].init?.headers?.Accept).toBe("text/event-stream");
    expect(calls[1].init?.headers?.["aitp-authorization"]).toBe("stream-authorization");
  });
});

describe("operator surfaces", () => {
  it("returns typed unhealthy dependency details from HTTP 503", async () => {
    const { fetch } = queueFetch([
      response(503, {
        gateway: "ok",
        clickhouse: "ok",
        postgres: "error",
        valkey: "ok",
        valkey_cache: "ok",
      }),
    ]);
    const health = await new RelayClient({ baseUrl: BASE, fetch }).health();
    expect(health.postgres).toBe("error");
  });

  it("uses Relay's live catalog and exact quote routes", async () => {
    const catalog = {
      schema_version: 1,
      revision: "r1",
      generated_at_ms: 1,
      models: [],
    };
    const quote = {
      model: "openai/gpt-4o",
      input_tokens: 1_000,
      output_tokens: 500,
      amount_micros: 7_500,
      pricing: {
        input_micro_usd_per_million: 2_500_000,
        output_micro_usd_per_million: 10_000_000,
        source: "provider_list",
      },
      catalog_revision: "r1",
    };
    const { fetch, calls } = queueFetch([
      response(200, { data: catalog }),
      response(200, { data: quote }),
    ]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    expect(await client.catalog()).toEqual(catalog);
    expect(await client.quote({ model: "openai/gpt-4o", input_tokens: 1_000, output_tokens: 500 })).toEqual(quote);
    expect(calls[0].init?.method).toBe("GET");
    expect(calls[1].url).toBe(`${BASE}/v1/relay/quotes`);
  });

  it("lists owner cost events with bounded encoded filters", async () => {
    const { fetch, calls } = queueFetch([
      response(200, { data: { items: [], next_cursor: null } }),
    ]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    expect(await client.costEvents({ state: "dead_lettered", before: REQUEST_ID, limit: 25 })).toEqual({
      items: [],
      next_cursor: null,
    });
    expect(calls[0].url).toBe(
      `${BASE}/v1/billing/cost-events?limit=25&state=dead_lettered&before=${REQUEST_ID}`,
    );
    await expect(client.costEvents({ limit: 101 })).rejects.toThrow(/between 1 and 100/);
  });
});

describe("binary multipart", () => {
  it("preserves file bytes and closes the generated boundary", async () => {
    const { fetch, calls } = queueFetch([response(200, { uploaded: true })]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    const result = await client.multipart<{ uploaded: boolean }>(
      "v1/files",
      new MultipartForm()
        .text("purpose", "batch")
        .file("file", "requests.jsonl", "application/jsonl", new Uint8Array([0, 1, 255])),
    );

    expect(result.uploaded).toBe(true);
    const contentType = calls[0].init?.headers?.["Content-Type"] ?? "";
    expect(contentType).toMatch(/^multipart\/form-data; boundary=relay-sdk-/);
    const boundary = contentType.split("boundary=")[1];
    const body = calls[0].init?.body;
    expect(body).toBeInstanceOf(Uint8Array);
    const bytes = body as Uint8Array;
    expect(Array.from(bytes).some((value, index) =>
      value === 0 && bytes[index + 1] === 1 && bytes[index + 2] === 255
    )).toBe(true);
    expect(new TextDecoder().decode(bytes.slice(-(`--${boundary}--\r\n`.length))))
      .toBe(`--${boundary}--\r\n`);
  });

  it("replays byte-identical multipart after one AITP challenge", async () => {
    const { fetch, calls } = queueFetch([
      response(402, {}, {
        "aitp-challenge": encodedChallenge(),
        "aitp-request-id": REQUEST_ID,
      }),
      response(200, { uploaded: true }),
    ]);
    const client = new RelayClient({ baseUrl: BASE, fetch });
    await client.paidMultipart(
      "v1/files",
      new MultipartForm().file(
        "file",
        "input.bin",
        "application/octet-stream",
        new Uint8Array([13, 10, 0, 255]),
      ),
      { authorize: () => "signed-upload" },
    );

    expect(calls).toHaveLength(2);
    expect(calls[1].init?.headers?.["aitp-authorization"]).toBe("signed-upload");
    expect(calls[1].init?.body).toEqual(calls[0].init?.body);
    expect(calls[1].init?.headers?.["Content-Type"])
      .toBe(calls[0].init?.headers?.["Content-Type"]);
  });

  it("rejects multipart header injection", () => {
    expect(() => new MultipartForm().file(
      "file",
      "bad\r\nX-Evil: yes",
      "application/octet-stream",
      new Uint8Array(),
    )).toThrow(/unsafe header/);
  });
});

describe("stream settlement handoff", () => {
  const chunk = `data: ${JSON.stringify({
    id: "chatcmpl-1",
    object: "chat.completion.chunk",
    created: 1,
    model: "gpt-4o",
    choices: [{ index: 0, finish_reason: null, delta: { content: "x" } }],
  })}\n\n`;

  it("hands an early drop to the caller's recovery queue", async () => {
    const abandoned: string[] = [];
    const { fetch } = queueFetch([streamResponse(chunk)]);
    const client = new RelayClient({
      baseUrl: BASE,
      fetch,
      onAbandonedStream: (requestId) => abandoned.push(requestId),
    });
    const stream = client.chatCompletionsStream(params());
    await stream.next();
    await stream.return();
    expect(abandoned).toEqual([REQUEST_ID]);
  });

  it("does not enqueue recovery after the DONE sentinel", async () => {
    const abandoned: string[] = [];
    const { fetch } = queueFetch([streamResponse(`${chunk}data: [DONE]\n\n`)]);
    const client = new RelayClient({
      baseUrl: BASE,
      fetch,
      onAbandonedStream: (requestId) => abandoned.push(requestId),
    });
    for await (const _ of client.chatCompletionsStream(params())) void _;
    expect(abandoned).toEqual([]);
  });
});

describe("client boundary validation", () => {
  it("rejects credentials, non-http schemes, and response overages", async () => {
    expect(() => new RelayClient({ baseUrl: "https://user:secret@relay.test" })).toThrow(/credentials/);
    expect(() => new RelayClient({ baseUrl: "file:///tmp/relay" })).toThrow(/HTTP/);

    const { fetch } = queueFetch([response(200, { status: "ok" })]);
    const client = new RelayClient({ baseUrl: BASE, fetch, maxResponseBytes: 5 });
    await expect(client.status()).rejects.toThrow(/exceeded/);
  });
});
