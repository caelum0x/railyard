import { describe, it, expect } from "vitest";
import {
  RelayClient,
  InsufficientCreditsError,
  AuthError,
  RelayApiError,
  relayModelTarget,
  relayFunctionTarget,
  type FetchLike,
  type FetchLikeResponse,
} from "../src/index.js";

const BASE = "http://localhost:3000";
const KEY = "sk-relay-test-123";

interface Recorded {
  url: string;
  method?: string;
  headers?: Record<string, string>;
  body?: string;
}

/** Build a JSON FetchLikeResponse. */
function jsonResponse(status: number, payload: unknown): FetchLikeResponse {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: () => "application/json" },
    json: async () => payload,
    text: async () => JSON.stringify(payload),
    body: null,
  };
}

/** Build an SSE FetchLikeResponse from a list of raw SSE frame strings. */
function sseResponse(frames: string[]): FetchLikeResponse {
  const text = frames.join("");
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      const encoder = new TextEncoder();
      // Emit in a couple of arbitrary chunks to exercise buffering.
      const mid = Math.floor(text.length / 2);
      controller.enqueue(encoder.encode(text.slice(0, mid)));
      controller.enqueue(encoder.encode(text.slice(mid)));
      controller.close();
    },
  });
  return {
    ok: true,
    status: 200,
    headers: { get: () => "text/event-stream" },
    json: async () => {
      throw new Error("not json");
    },
    text: async () => text,
    body: stream,
  };
}

/** A fetch mock that records the call and returns a queued response. */
function mockFetch(
  responder: (rec: Recorded) => FetchLikeResponse,
): { fetch: FetchLike; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const fetch: FetchLike = async (url, init) => {
    const rec: Recorded = {
      url,
      method: init?.method,
      headers: init?.headers,
      body: init?.body,
    };
    calls.push(rec);
    return responder(rec);
  };
  return { fetch, calls };
}

describe("RelayClient constructor", () => {
  it("throws without a baseUrl", () => {
    // @ts-expect-error intentionally invalid
    expect(() => new RelayClient({})).toThrow(/baseUrl/);
  });

  it("strips a trailing slash from baseUrl", async () => {
    const { fetch, calls } = mockFetch(() =>
      jsonResponse(200, chatPayload()),
    );
    const client = new RelayClient({ baseUrl: `${BASE}/`, apiKey: KEY, fetch });
    await client.chatCompletions(basicParams());
    expect(calls[0].url).toBe(`${BASE}/openai/v1/chat/completions`);
  });
});

describe("chatCompletions (JSON)", () => {
  it("posts to the OpenAI-compatible path with the correct request shape", async () => {
    const { fetch, calls } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });

    const res = await client.chatCompletions(basicParams());

    expect(calls).toHaveLength(1);
    const call = calls[0];
    expect(call.url).toBe(`${BASE}/openai/v1/chat/completions`);
    expect(call.method).toBe("POST");
    const body = JSON.parse(call.body!);
    expect(body.model).toBe("tensorzero::model_name::gpt-4o-mini");
    expect(body.messages).toEqual([{ role: "user", content: "Hi" }]);
    expect(body.stream).toBe(false);

    expect(res.choices[0].message.content).toBe("Hello there");
    expect(res.usage.total_tokens).toBe(9);
  });

  it("sends the Authorization bearer header and JSON content-type", async () => {
    const { fetch, calls } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await client.chatCompletions(basicParams());

    expect(calls[0].headers?.Authorization).toBe(`Bearer ${KEY}`);
    expect(calls[0].headers?.["Content-Type"]).toBe("application/json");
  });

  it("omits Authorization when no apiKey is set", async () => {
    const { fetch, calls } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, fetch });
    await client.chatCompletions(basicParams());
    expect(calls[0].headers?.Authorization).toBeUndefined();
  });

  it("passes through tensorzero:: extension params", async () => {
    const { fetch, calls } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await client.chatCompletions({
      ...basicParams(),
      "tensorzero::episode_id": "ep-1",
      "tensorzero::include_raw_usage": true,
    });
    const body = JSON.parse(calls[0].body!);
    expect(body["tensorzero::episode_id"]).toBe("ep-1");
    expect(body["tensorzero::include_raw_usage"]).toBe(true);
  });

  it("validates required model and messages", async () => {
    const { fetch } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await expect(
      // @ts-expect-error missing model
      client.chatCompletions({ messages: [{ role: "user", content: "x" }] }),
    ).rejects.toThrow(/model/);
    await expect(
      client.chatCompletions({ model: "m", messages: [] }),
    ).rejects.toThrow(/messages/);
  });

  it("rejects n != 1", async () => {
    const { fetch } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await expect(
      client.chatCompletions({ ...basicParams(), n: 2 }),
    ).rejects.toThrow(/n/);
  });
});

describe("chatCompletionsStream (SSE)", () => {
  it("sets stream:true and include_usage on the request", async () => {
    const { fetch, calls } = mockFetch(() =>
      sseResponse([doneFrame()]),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    // Drain the generator.
    for await (const _ of client.chatCompletionsStream(basicParams())) {
      void _;
    }
    const body = JSON.parse(calls[0].body!);
    expect(body.stream).toBe(true);
    expect(body.stream_options).toEqual({ include_usage: true });
    expect(calls[0].headers?.Accept).toBe("text/event-stream");
  });

  it("parses delta chunks and stops at [DONE]", async () => {
    const frames = [
      dataFrame({ role: "assistant", content: "" }),
      dataFrame({ content: "Hel" }),
      dataFrame({ content: "lo" }),
      usageFrame(),
      doneFrame(),
    ];
    const { fetch } = mockFetch(() => sseResponse(frames));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });

    const chunks = [];
    for await (const chunk of client.chatCompletionsStream(basicParams())) {
      chunks.push(chunk);
    }

    // 3 delta frames + 1 usage frame; [DONE] terminates without a chunk.
    expect(chunks).toHaveLength(4);
    const text = chunks
      .map((c) => c.choices[0]?.delta.content ?? "")
      .join("");
    expect(text).toBe("Hello");

    const finalChunk = chunks[chunks.length - 1];
    expect(finalChunk.usage?.total_tokens).toBe(7);
  });

  it("handles data chunks split across network reads", async () => {
    // sseResponse deliberately splits the byte stream in the middle.
    const frames = [dataFrame({ content: "abc" }), dataFrame({ content: "def" }), doneFrame()];
    const { fetch } = mockFetch(() => sseResponse(frames));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    const out: string[] = [];
    for await (const chunk of client.chatCompletionsStream(basicParams())) {
      out.push(chunk.choices[0]?.delta.content ?? "");
    }
    expect(out.join("")).toBe("abcdef");
  });
});

describe("error handling", () => {
  it("maps 402 to InsufficientCreditsError", async () => {
    const { fetch } = mockFetch(() =>
      jsonResponse(402, {
        error: {
          message: "insufficient credits",
          type: "insufficient_credits",
          code: "insufficient_credits",
        },
      }),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await expect(client.chatCompletions(basicParams())).rejects.toBeInstanceOf(
      InsufficientCreditsError,
    );
  });

  it("maps 401 to AuthError", async () => {
    const { fetch } = mockFetch(() =>
      jsonResponse(401, { error: { message: "bad key", type: "unauthorized" } }),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: "wrong", fetch });
    const err = await client.chatCompletions(basicParams()).catch((e) => e);
    expect(err).toBeInstanceOf(AuthError);
    expect((err as AuthError).status).toBe(401);
  });

  it("maps other non-2xx to RelayApiError", async () => {
    const { fetch } = mockFetch(() =>
      jsonResponse(500, { error: { message: "boom", type: "internal" } }),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    const err = await client.chatCompletions(basicParams()).catch((e) => e);
    expect(err).toBeInstanceOf(RelayApiError);
    expect((err as RelayApiError).status).toBe(500);
    expect((err as RelayApiError).message).toBe("boom");
  });
});

describe("models()", () => {
  it("returns configured known models with relay targets", () => {
    const client = new RelayClient({
      baseUrl: BASE,
      apiKey: KEY,
      knownModels: ["gpt-4o-mini", "tensorzero::function_name::draft"],
    });
    const models = client.models();
    expect(models).toEqual([
      {
        id: "gpt-4o-mini",
        object: "model",
        kind: "model",
        relay_target: "tensorzero::model_name::gpt-4o-mini",
      },
      {
        id: "tensorzero::function_name::draft",
        object: "model",
        kind: "model",
        relay_target: "tensorzero::function_name::draft",
      },
    ]);
  });

  it("defaults to an empty catalog when none configured", () => {
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY });
    expect(client.models()).toEqual([]);
  });
});

describe("credits()", () => {
  it("reports hasCredits:false when the probe hits 402", async () => {
    const { fetch, calls } = mockFetch(() =>
      jsonResponse(402, {
        error: { message: "insufficient credits", type: "insufficient_credits" },
      }),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    const status = await client.credits("gpt-4o-mini");
    expect(status.hasCredits).toBe(false);
    expect(status.balance).toBeNull();
    expect(status.reason).toMatch(/insufficient/);
    // The probe uses max_tokens:1.
    expect(JSON.parse(calls[0].body!).max_tokens).toBe(1);
  });

  it("reports hasCredits:true when the probe succeeds", async () => {
    const { fetch } = mockFetch(() => jsonResponse(200, chatPayload()));
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    const status = await client.credits("gpt-4o-mini");
    expect(status).toEqual({ hasCredits: true, balance: null });
  });

  it("rethrows non-billing errors", async () => {
    const { fetch } = mockFetch(() =>
      jsonResponse(500, { error: { message: "boom" } }),
    );
    const client = new RelayClient({ baseUrl: BASE, apiKey: KEY, fetch });
    await expect(client.credits("gpt-4o-mini")).rejects.toBeInstanceOf(
      RelayApiError,
    );
  });
});

describe("target helpers", () => {
  it("builds model and function targets, leaving explicit ones intact", () => {
    expect(relayModelTarget("gpt-4o")).toBe("tensorzero::model_name::gpt-4o");
    expect(relayModelTarget("tensorzero::model_name::x")).toBe(
      "tensorzero::model_name::x",
    );
    expect(relayFunctionTarget("draft")).toBe(
      "tensorzero::function_name::draft",
    );
  });
});

// ---- fixtures -------------------------------------------------------------

function basicParams() {
  return {
    model: "tensorzero::model_name::gpt-4o-mini",
    messages: [{ role: "user" as const, content: "Hi" }],
  };
}

function chatPayload() {
  return {
    id: "chatcmpl-1",
    episode_id: "ep-1",
    object: "chat.completion",
    created: 1700000000,
    model: "gpt-4o-mini",
    choices: [
      {
        index: 0,
        finish_reason: "stop",
        message: { role: "assistant", content: "Hello there" },
      },
    ],
    usage: { prompt_tokens: 4, completion_tokens: 5, total_tokens: 9 },
  };
}

function dataFrame(delta: Record<string, unknown>): string {
  const chunk = {
    id: "chatcmpl-1",
    object: "chat.completion.chunk",
    created: 1700000000,
    model: "gpt-4o-mini",
    choices: [{ index: 0, finish_reason: null, delta }],
  };
  return `data: ${JSON.stringify(chunk)}\n\n`;
}

function usageFrame(): string {
  const chunk = {
    id: "chatcmpl-1",
    object: "chat.completion.chunk",
    created: 1700000000,
    model: "gpt-4o-mini",
    choices: [],
    usage: { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 },
  };
  return `data: ${JSON.stringify(chunk)}\n\n`;
}

function doneFrame(): string {
  return "data: [DONE]\n\n";
}
