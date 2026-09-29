// Small HTTP helpers shared by route handlers.

/** Session-authenticated writes must come from the console's own origin (CSRF). */
export function sameOrigin(req: Request, consoleUrl: string): boolean {
  const origin = req.headers.get("origin");
  if (origin) return origin === new URL(consoleUrl).origin;
  const referer = req.headers.get("referer");
  return !!referer && new URL(referer).origin === new URL(consoleUrl).origin;
}

export function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store", ...headers } });
}

export function cookie(name: string, value: string, maxAgeSeconds: number): string {
  return `${name}=${value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${maxAgeSeconds}`;
}

export function readCookie(req: Request, name: string): string | null {
  const raw = req.headers.get("cookie") ?? "";
  for (const part of raw.split(/;\s*/)) {
    const i = part.indexOf("=");
    if (i > 0 && part.slice(0, i) === name) return part.slice(i + 1);
  }
  return null;
}
