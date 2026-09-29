import { connectCredential } from "../../../../lib/ai/provision.ts";
import { deps } from "../../../../lib/deps.ts";
import { sameOrigin } from "../../../../lib/http.ts";

export const dynamic = "force-dynamic";

// The one place a provider key is typed (PY1.4): a form post from /connect/ai,
// authorised by the one-time link, never by a session or a flag.
export async function POST(req: Request) {
  const { cfg, repo } = deps();
  if (!sameOrigin(req, cfg.consoleUrl)) return new Response("Forbidden", { status: 403 });
  const back = (q: string) => Response.redirect(`${cfg.consoleUrl}/connect/ai?${q}`, 303);
  if (!cfg.aiGateway) return back("error=not_configured");
  const form = await req.formData().catch(() => null);
  const token = String(form?.get("t") ?? "");
  const r = await connectCredential(
    { repo, credentialsKey: cfg.aiGateway.credentialsKey, now: () => new Date() },
    token,
    String(form?.get("provider") ?? ""),
    String(form?.get("key") ?? ""),
  );
  return r.ok ? back(`done=${r.provider}&hint=${encodeURIComponent(r.hint)}`) : back(`t=${encodeURIComponent(token)}&error=${r.status === 410 ? "expired" : "invalid"}`);
}
