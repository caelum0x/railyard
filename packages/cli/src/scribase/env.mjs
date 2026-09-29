// Scribase project settings for `railyard deploy` (Wave P PY1.2).
//
// `npx scribase init` writes the project's API URL and anon key into the app's
// env file (.env.local for Next.js and Vite, .env for Expo and plain Node).
// Both are public by design: they ship in the app bundle, and row-level
// security guards the data. This module reads exactly those names, so a
// deploy from a Scribase project folder needs nothing pasted. Secrets (the
// service role key, anything named SECRET or PASSWORD, a service-role JWT
// under any name) are never read into the build or the Worker; they are
// reported by name only.
//
// A Scribase branch has its own settings in `.scribase/branches/<branch>.env`
// (written by the branch hook; PS2.0 lands the call). A preview of that git
// branch uses them; otherwise the main settings.

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const PREFIXES = ["", "NEXT_PUBLIC_", "VITE_", "EXPO_PUBLIC_", "PUBLIC_"];
const BASES = ["SUPABASE_URL", "SUPABASE_ANON_KEY", "SCRIBASE_URL", "SCRIBASE_ANON_KEY"];
export const PUBLIC_NAMES = new Set(PREFIXES.flatMap((p) => BASES.map((b) => `${p}${b}`)));
const SECRET_NAME = /(SECRET|PASSWORD|PASSWD|PRIVATE|SERVICE_ROLE|ACCESS_TOKEN|_PASS$)/;
const SECRET_VALUE = [/^sb_secret_/, /^scb_(agt|pat|mt)_/, /-----BEGIN [A-Z ]*PRIVATE KEY-----/, /^[a-z][a-z0-9+.-]*:\/\/[^/\s:@]+:[^/\s@]+@/i];
const KEY = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$/;

function parseEnv(text) {
  const out = {};
  for (const line of text.split(/\r?\n/)) {
    const m = KEY.exec(line);
    if (!m) continue;
    let v = m[2].trim();
    if (v.startsWith('"')) {
      try {
        v = JSON.parse(v);
      } catch {
        v = v.slice(1, -1);
      }
    } else if (v.startsWith("'") && v.endsWith("'")) v = v.slice(1, -1);
    out[m[1]] = v;
  }
  return out;
}

function jwtRole(value) {
  const parts = value.split(".");
  if (parts.length !== 3 || !value.startsWith("eyJ")) return null;
  try {
    return JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8")).role ?? null;
  } catch {
    return null;
  }
}

export function isSecret(name, value) {
  if (SECRET_NAME.test(name)) return true;
  if (SECRET_VALUE.some((re) => re.test(value))) return true;
  return jwtRole(value) === "service_role";
}

export function branchFile(dir, branch) {
  const slug = String(branch).toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "");
  return join(dir, ".scribase", "branches", `${slug}.env`);
}

/**
 * The Scribase settings to use for a deploy of `dir` on `branch`:
 * { found, branch, values, names, refused }. `values` holds public names only.
 */
export function projectEnv(dir, branch = "main") {
  const files = [".env", ".env.local"].map((f) => join(dir, f));
  const bf = branch && branch !== "main" && branch !== "master" ? branchFile(dir, branch) : null;
  const useBranch = !!bf && existsSync(bf);
  if (useBranch) files.push(bf);
  const merged = {};
  for (const f of files) if (existsSync(f)) Object.assign(merged, parseEnv(readFileSync(f, "utf8")));
  const values = {};
  const refused = [];
  for (const [name, value] of Object.entries(merged)) {
    const looksScribase = PUBLIC_NAMES.has(name) || /SUPABASE|SCRIBASE/.test(name);
    if (!looksScribase) continue;
    if (isSecret(name, value)) refused.push(name);
    else if (PUBLIC_NAMES.has(name) && value) values[name] = value;
  }
  const names = Object.keys(values).sort();
  return { found: names.length > 0, branch: useBranch, values, names, refused: refused.sort() };
}

/** wrangler `--var NAME:value` arguments, so a Next.js server reads them at run time. */
export function wranglerVars(values) {
  return Object.entries(values).flatMap(([k, v]) => ["--var", `${k}:${v}`]);
}
