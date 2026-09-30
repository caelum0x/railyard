// The service's no-login pages (replay player, heatmap overlay) and their
// static files. A page is plain HTML whose script and styles are files under
// public/player (no inline script), with a strict content security policy:
// only this service's scripts run; the recorded page's styles, images and
// fonts may load (a replay needs them); the page is framed only by the
// console's origin (PA_CONSOLE_ORIGIN) and sends no referrer, so the signed
// link never leaks to the recorded site's asset hosts.

import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Context } from "hono";

export const PUBLIC_DIR = join(dirname(fileURLToPath(import.meta.url)), "../../public/player");

const ASSETS: Record<string, string> = {
  "vendor/rrweb-player/rrweb-player.min.js": "application/javascript; charset=utf-8",
  "vendor/rrweb-player/style.css": "text/css; charset=utf-8",
  "vendor/simpleheat/simpleheat.js": "application/javascript; charset=utf-8",
  "replay.js": "application/javascript; charset=utf-8",
  "heatmap.js": "application/javascript; charset=utf-8",
  "player.css": "text/css; charset=utf-8",
};

/** The SDK's lazily loaded parts, served at /static/<name>.js as the SDK expects from its host. */
export const SDK_FILES = new Set(["lazy-recorder.js", "surveys.js", "dead-clicks-autocapture.js"]);

const cache = new Map<string, Buffer>();
async function file(path: string): Promise<Buffer> {
  const hit = cache.get(path);
  if (hit) return hit;
  const b = await readFile(join(PUBLIC_DIR, path));
  cache.set(path, b);
  return b;
}

export async function assetResponse(c: Context, name: string): Promise<Response> {
  const type = ASSETS[name];
  if (!type) return c.text("Not found", 404);
  return c.body(new Uint8Array(await file(name)), 200, { "content-type": type, "cache-control": "public, max-age=86400", "x-content-type-options": "nosniff" });
}

export async function sdkResponse(c: Context, name: string): Promise<Response> {
  if (!SDK_FILES.has(name)) return c.text("// not found", 404);
  return c.body(new Uint8Array(await file(`sdk/${name}`)), 200, {
    "content-type": "application/javascript; charset=utf-8",
    "cache-control": "public, max-age=3600",
    "access-control-allow-origin": "*",
    "x-content-type-options": "nosniff",
  });
}

export function frameAncestors(env: Record<string, string | undefined> = process.env): string {
  const list = (env.PA_CONSOLE_ORIGIN ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter((s) => /^https?:\/\/[A-Za-z0-9.-]+(:\d+)?$/.test(s));
  return list.length ? list.join(" ") : "'self'";
}

export function pageHeaders(): Record<string, string> {
  return {
    "content-type": "text/html; charset=utf-8",
    "cache-control": "no-store",
    "referrer-policy": "no-referrer",
    "x-content-type-options": "nosniff",
    "content-security-policy": [
      "default-src 'none'",
      "script-src 'self'",
      "style-src 'self' 'unsafe-inline' https: http:",
      "img-src * data: blob:",
      "font-src * data:",
      "media-src * data: blob:",
      "connect-src 'self'",
      "frame-src 'self' about: blob: https: http:",
      `frame-ancestors ${frameAncestors()}`,
      "base-uri 'none'",
      "form-action 'none'",
    ].join("; "),
  };
}

export const escapeHtml = (s: string): string => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");

/** A page whose body carries `data` as data- attributes for its script. */
export function pageHtml(opts: { title: string; scripts: string[]; styles: string[]; data: Record<string, string> }): string {
  const attrs = Object.entries(opts.data)
    .map(([k, v]) => ` data-${k}="${escapeHtml(v)}"`)
    .join("");
  const links = opts.styles.map((s) => `<link rel="stylesheet" href="/player/assets/${s}">`).join("");
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="robots" content="noindex"><title>${escapeHtml(opts.title)}</title>${links}</head><body${attrs}><main id="stage"><p id="status">Loading...</p></main>${opts.scripts.map((s) => `<script src="/player/assets/${s}"></script>`).join("")}</body></html>`;
}
