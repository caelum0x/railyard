// The org token from `railyard login`, stored for this user only (0600).
// The token is never printed and never taken from a flag.

import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export function credentialsPath(env = process.env) {
  const base = env.RAILYARD_CONFIG_DIR || join(env.XDG_CONFIG_HOME || join(homedir(), ".config"), "railyard");
  return join(base, "credentials.json");
}

export function saveCredentials(creds, env = process.env) {
  const p = credentialsPath(env);
  mkdirSync(dirname(p), { recursive: true, mode: 0o700 });
  writeFileSync(p, JSON.stringify(creds, null, 2), { mode: 0o600 });
  chmodSync(p, 0o600);
  return p;
}

export function loadCredentials(env = process.env) {
  const p = credentialsPath(env);
  if (!existsSync(p)) return null;
  try {
    const c = JSON.parse(readFileSync(p, "utf8"));
    return typeof c.token === "string" && typeof c.console === "string" ? c : null;
  } catch {
    return null;
  }
}

/** Replace any secret-looking value in text before it reaches the terminal. */
export function redact(text, secrets = []) {
  let out = String(text);
  for (const s of secrets) if (s && s.length >= 8) out = out.split(s).join("[redacted]");
  return out.replace(/\bry_[A-Za-z0-9_-]{20,}\b/g, "[redacted]");
}
