// Keys and the secure connect page for the AI gateway (Wave P PY1.4).
//
// - A project's key (`ry_ai_...`) is returned once, to the Scribase control
//   plane, which stores it as the project's function secret. Only its
//   SHA-256 is kept here.
// - The customer's provider key is typed once, on the connect page, reached
//   through a one-time link (15 minutes). It is sealed before it is stored and
//   never shown again (only a four-character hint).

import { randomToken, sha256Hex } from "../crypto.ts";
import type { Repo } from "../repo.ts";
import type { AiKey, AiProvider } from "../types.ts";
import { PROVIDERS } from "./gateway.ts";
import { hint, seal } from "./seal.ts";

export const CONNECT_TTL_SECONDS = 15 * 60;
export const DEFAULT_CAPS = Object.freeze({ daily: 1_000_000, monthly: 20_000_000 });

export async function issueAiKey(
  repo: Repo,
  orgId: string,
  label: string,
  caps: { daily: number; monthly: number } = DEFAULT_CAPS,
): Promise<{ token: string; key: AiKey }> {
  const token = `ry_ai_${randomToken(32)}`;
  const key = await repo.createAiKey({
    org_id: orgId,
    label: label.slice(0, 80),
    key_hash: await sha256Hex(token),
    daily_token_cap: caps.daily,
    monthly_token_cap: caps.monthly,
  });
  return { token, key };
}

export async function issueConnectToken(repo: Repo, orgId: string, now: Date): Promise<string> {
  const token = `ry_ct_${randomToken(32)}`;
  await repo.createConnectToken({
    token_hash: await sha256Hex(token),
    org_id: orgId,
    purpose: "ai",
    expires_at: new Date(now.getTime() + CONNECT_TTL_SECONDS * 1000).toISOString(),
    used_at: null,
  });
  return token;
}

export type ConnectResult = { ok: true; provider: AiProvider; hint: string } | { ok: false; status: number; message: string };

/** Stores the customer's provider key for the token's org. The token works once. */
export async function connectCredential(
  deps: { repo: Repo; credentialsKey: string; now: () => Date },
  token: string,
  provider: string,
  secret: string,
): Promise<ConnectResult> {
  if (!PROVIDERS.includes(provider as AiProvider)) return { ok: false, status: 400, message: "Choose a provider from the list." };
  const value = secret.trim();
  if (value.length < 8 || value.length > 1024 || /\s/.test(value)) return { ok: false, status: 400, message: "That does not look like a provider key." };
  if (!/^ry_ct_[A-Za-z0-9_-]{20,}$/.test(token)) return { ok: false, status: 400, message: "This link is not valid. Start again from your terminal." };
  const now = deps.now();
  const t = await deps.repo.consumeConnectToken(await sha256Hex(token), now.toISOString());
  if (!t) return { ok: false, status: 410, message: "This link was used already or expired. Start again from your terminal." };
  const p = provider as AiProvider;
  const sealed = await seal(deps.credentialsKey, `${t.org_id}:${p}`, value);
  await deps.repo.saveAiCredential({ org_id: t.org_id, provider: p, nonce: sealed.nonce, ciphertext: sealed.ciphertext, key_hint: hint(value) });
  return { ok: true, provider: p, hint: hint(value) };
}
