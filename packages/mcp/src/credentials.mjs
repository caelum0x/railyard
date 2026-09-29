// Reads the sign-in saved by `railyard login` (same file as the CLI). The MCP
// server never takes a token as an argument or prints one.
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export function loadCredentials(env = process.env) {
  const base = env.RAILYARD_CONFIG_DIR || join(env.XDG_CONFIG_HOME || join(homedir(), ".config"), "railyard");
  const p = join(base, "credentials.json");
  if (!existsSync(p)) return null;
  try {
    const c = JSON.parse(readFileSync(p, "utf8"));
    return typeof c.token === "string" && typeof c.console === "string" ? c : null;
  } catch {
    return null;
  }
}
