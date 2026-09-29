// The AI gateway for Scribase projects (Wave P PY1.4): BYOK only (ADR RY-0003).
//
// POST /api/v1/ai/chat/completions (OpenAI-compatible) with
// `authorization: Bearer ry_ai_...` (the project's key; Scribase keeps it as a
// function secret, never in a terminal or chat).
//
// 1. The key must exist, not be revoked, and its org's add-on must be active.
// 2. The model names its provider (`anthropic/claude-...` or
//    `tensorzero::model_name::anthropic::claude-...`), and the org must have
//    connected its own account for that provider on the secure connect page.
//    Otherwise the answer is 403 byok_required: there is no path to our keys.
// 3. Daily and monthly token caps per key (429 with the period).
// 4. The call goes to Relay with the customer's credential in
//    `tensorzero::credentials` for that provider and a refusal marker in EVERY
//    other provider slot: Relay lets a caller's credential win over its own,
//    so no stored or environment key of ours can ever be used, not even on
//    failover (apps/relay/crates/gateway/src/provider_keys.rs, and the slot
//    list is checked against that file by a test).
// 5. Usage and one trace row are recorded per call.

import { sha256Hex } from "../crypto.ts";
import { access } from "../ladder.ts";
import type { Repo } from "../repo.ts";
import type { AiKey, AiProvider } from "../types.ts";
import { MESSAGES } from "./messages.ts";
import { open } from "./seal.ts";

/** Relay's `dynamic::<slot>` credential names, per provider. */
export const PROVIDER_SLOTS: Readonly<Record<AiProvider, string>> = Object.freeze({
  openai: "relay_openai_api_key",
  anthropic: "relay_anthropic_api_key",
  groq: "relay_groq_api_key",
  openrouter: "relay_openrouter_api_key",
});
export const PROVIDERS = Object.keys(PROVIDER_SLOTS) as AiProvider[];
/** What every slot without the customer's own key carries: the provider refuses it. */
export const NOT_CONNECTED = "byok-not-connected";

export interface GatewayDeps {
  repo: Repo;
  relayUrl: string;
  relayKey: string;
  credentialsKey: string;
  consoleUrl: string;
  fetchImpl?: typeof fetch;
  now: () => Date;
}

export interface GatewayResult {
  status: number;
  body: unknown;
  headers?: Record<string, string>;
}

const err = (status: number, code: string, message: string, extra: Record<string, unknown> = {}): GatewayResult => ({
  status,
  body: { error: { code, message, ...extra } },
});

/** The provider a model names, and the model id without the provider. */
export function parseModel(model: unknown): { provider: AiProvider; id: string } | null {
  if (typeof model !== "string" || model.length > 200) return null;
  const t = /^tensorzero::model_name::([a-z]+)::(.+)$/.exec(model);
  const s = /^([a-z]+)\/(.+)$/.exec(model);
  const m = t ?? s;
  if (!m) return null;
  const provider = m[1] as AiProvider;
  return PROVIDERS.includes(provider) && m[2] ? { provider, id: m[2] } : null;
}

/** The credentials map sent to Relay: the customer's key for one slot, a refusal marker for all others. */
export function credentialsFor(provider: AiProvider, secret: string): Record<string, string> {
  return Object.fromEntries(PROVIDERS.map((p) => [PROVIDER_SLOTS[p], p === provider ? secret : NOT_CONNECTED]));
}

export async function keyHash(token: string): Promise<string> {
  return sha256Hex(token);
}

function utcDay(d: Date): string {
  return d.toISOString().slice(0, 10);
}

function monthStart(d: Date): string {
  return `${d.toISOString().slice(0, 7)}-01`;
}

async function authorize(deps: GatewayDeps, header: string | null): Promise<AiKey | GatewayResult> {
  const m = /^Bearer\s+(ry_ai_[A-Za-z0-9_-]{20,})$/.exec(header ?? "");
  if (!m) return err(401, "unauthorized", MESSAGES.unauthorized_header);
  const key = await deps.repo.findAiKeyByHash(await keyHash(m[1]));
  if (!key) return err(401, "unauthorized", MESSAGES.unauthorized_key);
  const a = access(await deps.repo.getEntitlement(key.org_id), deps.now());
  if (a.state !== "active") return err(402, "addon_required", MESSAGES.addon);
  return key;
}

/** One chat completion, BYOK only. */
export async function chatCompletion(deps: GatewayDeps, authorization: string | null, raw: string): Promise<GatewayResult> {
  const key = await authorize(deps, authorization);
  if ("status" in key) return key;
  let body: Record<string, unknown>;
  try {
    body = JSON.parse(raw);
  } catch {
    return err(400, "invalid_json", MESSAGES.json);
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) return err(400, "invalid_json", MESSAGES.json_object);
  if (body.stream === true) return err(400, "stream_not_supported", MESSAGES.stream);
  if ("tensorzero::credentials" in body) return err(400, "credentials_not_accepted", MESSAGES.credentials);
  const model = parseModel(body.model);
  if (!model) return err(400, "model_needs_provider", MESSAGES.model(PROVIDERS));
  const cred = await deps.repo.getAiCredential(key.org_id, model.provider);
  if (!cred) {
    return err(403, "byok_required", MESSAGES.byok(model.provider), {
      connect: `${deps.consoleUrl}/connect/ai`,
    });
  }
  const now = deps.now();
  const [today, month] = await Promise.all([deps.repo.aiTokensSince(key.id, utcDay(now)), deps.repo.aiTokensSince(key.id, monthStart(now))]);
  if (today >= key.daily_token_cap) return { ...err(429, "cap_reached", MESSAGES.day, { period: "day" }), headers: { "retry-after": "3600" } };
  if (month >= key.monthly_token_cap) return { ...err(429, "cap_reached", MESSAGES.month, { period: "month" }), headers: { "retry-after": "86400" } };
  const secret = await open(deps.credentialsKey, `${key.org_id}:${model.provider}`, cred.nonce, cred.ciphertext);
  const relayBody = {
    ...body,
    model: `tensorzero::model_name::${model.provider}::${model.id}`,
    "tensorzero::credentials": credentialsFor(model.provider, secret),
    "tensorzero::tags": { tenant: "scribase", railyard_org: key.org_id, railyard_ai_key: key.id },
  };
  const started = Date.now();
  const res = await (deps.fetchImpl ?? fetch)(`${deps.relayUrl.replace(/\/$/, "")}/openai/v1/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${deps.relayKey}` },
    body: JSON.stringify(relayBody),
  });
  const text = await res.text();
  let answer: unknown = null;
  try {
    answer = text ? JSON.parse(text) : null;
  } catch {
    answer = { error: { code: "upstream_not_json", message: MESSAGES.not_json } };
  }
  const usage = (answer as { usage?: { prompt_tokens?: number; completion_tokens?: number } } | null)?.usage ?? {};
  const input = Number.isFinite(usage.prompt_tokens) ? Number(usage.prompt_tokens) : 0;
  const output = Number.isFinite(usage.completion_tokens) ? Number(usage.completion_tokens) : 0;
  await deps.repo.addAiUsage(key.id, utcDay(now), input + output);
  await deps.repo.addAiTrace({
    key_id: key.id,
    at: now.toISOString(),
    model: model.id,
    provider: model.provider,
    status: res.status,
    input_tokens: input,
    output_tokens: output,
    latency_ms: Date.now() - started,
  });
  // Never echo the credential, even if a provider error message contained it.
  const safe = JSON.stringify(answer ?? {}).split(secret).join("[redacted]");
  return { status: res.status, body: JSON.parse(safe) };
}
