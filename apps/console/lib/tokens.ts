// CLI sign-in (device flow, RFC 8628 shape) and org-scoped bearer tokens.
// Only SHA-256 hashes of device codes and tokens are stored. A token is
// shown once, to the CLI that polled for it, never on a web page.

import { randomToken, sha256Hex } from "./crypto.ts";
import type { Repo } from "./repo.ts";

export const DEVICE_TTL_SECONDS = 600;
export const POLL_INTERVAL_SECONDS = 3;
const USER_CODE_ALPHABET = "BCDFGHJKLMNPQRSTVWXZ";

function userCode(): string {
  const buf = new Uint8Array(8);
  crypto.getRandomValues(buf);
  const chars = [...buf].map((b) => USER_CODE_ALPHABET[b % USER_CODE_ALPHABET.length]);
  return `${chars.slice(0, 4).join("")}-${chars.slice(4).join("")}`;
}

export async function startDevice(repo: Repo, consoleUrl: string, now: Date) {
  const deviceCode = randomToken(32);
  const code = userCode();
  await repo.createDeviceCode({
    device_code_hash: await sha256Hex(deviceCode),
    user_code: code,
    org_id: null,
    user_id: null,
    approved_at: null,
    consumed_at: null,
    expires_at: new Date(now.getTime() + DEVICE_TTL_SECONDS * 1000).toISOString(),
  });
  return {
    device_code: deviceCode,
    user_code: code,
    verification_uri: `${consoleUrl}/device`,
    verification_uri_complete: `${consoleUrl}/device?code=${code}`,
    expires_in: DEVICE_TTL_SECONDS,
    interval: POLL_INTERVAL_SECONDS,
  };
}

export async function approveDevice(repo: Repo, rawCode: string, orgId: string, userId: string, now: Date): Promise<boolean> {
  const code = rawCode.trim().toUpperCase();
  if (!/^[A-Z]{4}-[A-Z]{4}$/.test(code)) return false;
  return repo.approveDevice(code, orgId, userId, now.toISOString());
}

export type PollResult =
  | { status: 200; body: { access_token: string; token_type: "bearer"; org_id: string } }
  | { status: 400; body: { error: "authorization_pending" | "expired_token" | "invalid_grant" } };

export async function pollDevice(repo: Repo, deviceCode: string, now: Date): Promise<PollResult> {
  const hash = await sha256Hex(deviceCode);
  const dc = await repo.findDeviceByHash(hash);
  if (!dc || dc.consumed_at) return { status: 400, body: { error: "invalid_grant" } };
  if (Date.parse(dc.expires_at) <= now.getTime()) return { status: 400, body: { error: "expired_token" } };
  if (!dc.approved_at || !dc.org_id || !dc.user_id) return { status: 400, body: { error: "authorization_pending" } };
  if (!(await repo.consumeDevice(hash, now.toISOString()))) return { status: 400, body: { error: "invalid_grant" } };
  const token = `ry_${randomToken(32)}`;
  await repo.createToken({ org_id: dc.org_id, user_id: dc.user_id, token_hash: await sha256Hex(token), label: "cli" });
  return { status: 200, body: { access_token: token, token_type: "bearer", org_id: dc.org_id } };
}

/** The org a bearer token acts for, or null. A token only ever reads its own org. */
export async function orgForBearer(repo: Repo, authorization: string | null): Promise<{ orgId: string; userId: string } | null> {
  const m = /^Bearer\s+(ry_[A-Za-z0-9_-]{20,})$/.exec(authorization ?? "");
  if (!m) return null;
  const t = await repo.findTokenByHash(await sha256Hex(m[1]));
  return t ? { orgId: t.org_id, userId: t.user_id } : null;
}
