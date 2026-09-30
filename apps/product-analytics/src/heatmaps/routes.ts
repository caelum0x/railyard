// Click maps and scroll depth (A3; handed over by A1).
//   Console: /v1/projects/:id/heatmaps?url&type&from&to[&viewport_min&viewport_max]
//            /v1/projects/:id/heatmaps/urls, /v1/projects/:id/heatmaps/link {url}
//   Link:    /player/heatmap?r&p&exp&sig    the overlay page (the page in a frame, the map on top)
//            /player/heatmap/data?...        its data
// Collection is switched on with heatmaps_enabled in /recording-settings.

import { Hono, type Context } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { pageHeaders, pageHtml } from "../replay/player-pages.ts";
import { publicBase } from "../replay/routes.ts";
import { ApiError, consoleGuard, jsonBody, plainErrors, projectOf, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { signLink, verifyLink } from "../replay/shared/signed-link.ts";
import { heatmapPoints, heatmapUrls, parseHeatmapQuery, scrollDepth } from "./query.ts";

const encodeUrl = (u: string) => Buffer.from(u, "utf8").toString("base64url");
const decodeUrl = (s: string) => Buffer.from(s, "base64url").toString("utf8");

export function heatmapsRoutes(deps: ServiceDeps): Hono {
  const app = new Hono();

  const linked = (c: Context) => {
    if (!deps.consoleSecret) return null;
    const v = verifyLink(deps.consoleSecret, new URL(c.req.url).searchParams, deps.now());
    if (!v || !v.resource.startsWith("heatmap:")) return null;
    return { project: v.project, url: decodeUrl(v.resource.slice("heatmap:".length)) };
  };

  app.get("/player/heatmap", (c) => {
    const l = linked(c);
    if (!l) return c.body(pageHtml({ title: "Link expired", scripts: [], styles: ["player.css"], data: {} }).replace("Loading...", "This link has expired or is not valid. Open the map again from the console."), 403, pageHeaders());
    const q = new URL(c.req.url).searchParams;
    const signed = new URLSearchParams({ r: q.get("r")!, p: q.get("p")!, exp: q.get("exp")!, sig: q.get("sig")! });
    for (const k of ["type", "from", "to", "viewport_min", "viewport_max"]) if (q.get(k)) signed.set(k, q.get(k)!);
    const width = Math.min(2560, Math.max(320, Number(q.get("w") ?? "1280") || 1280));
    return c.body(
      pageHtml({ title: "Click map", scripts: ["vendor/simpleheat/simpleheat.js", "heatmap.js"], styles: ["player.css"], data: { page: l.url, data: `/player/heatmap/data?${signed}`, width: String(width) } }),
      200,
      pageHeaders(),
    );
  });

  app.get("/player/heatmap/data", async (c) => {
    const l = linked(c);
    if (!l) return c.json({ error: "This link has expired or is not valid." }, 403);
    const q = new URL(c.req.url).searchParams;
    q.set("url", l.url);
    try {
      const query = parseHeatmapQuery(q, deps.now());
      const [points, scroll] = await Promise.all([heatmapPoints(deps.db, l.project, query), scrollDepth(deps.db, l.project, query)]);
      return c.json({ ...points, scroll }, 200, { "cache-control": "no-store", "referrer-policy": "no-referrer" });
    } catch (err) {
      if (err instanceof ApiError) return c.json({ error: err.message }, err.status);
      throw err;
    }
  });

  const api = new Hono<ConsoleEnv>();
  const P = "/v1/projects/:id/heatmaps";
  const guard = consoleGuard(deps);
  api.use(P, guard);
  api.use(`${P}/*`, guard);
  plainErrors(api, deps.log, "heatmaps");

  api.get(P, async (c) => {
    const query = parseHeatmapQuery(new URL(c.req.url).searchParams, deps.now());
    const [points, scroll] = await Promise.all([heatmapPoints(deps.db, projectOf(c), query), scrollDepth(deps.db, projectOf(c), query)]);
    return c.json({ ...points, scroll });
  });

  api.get(`${P}/urls`, async (c) => c.json({ urls: await heatmapUrls(deps.db, projectOf(c), deps.now()) }));

  api.post(`${P}/link`, async (c) => {
    const b = await jsonBody(c);
    const q = new URLSearchParams();
    for (const k of ["url", "type", "from", "to", "viewport_min", "viewport_max"]) if (b[k] !== undefined && b[k] !== null) q.set(k, String(b[k]));
    const query = parseHeatmapQuery(q, deps.now());
    const resource = `heatmap:${encodeUrl(query.url)}`;
    if (resource.length > 2056) throw new ApiError(400, "That address is too long for a link.");
    const link = signLink(deps.consoleSecret!, resource, projectOf(c), deps.now());
    const extra = new URLSearchParams({ type: query.type, from: query.from.toISOString(), to: query.to.toISOString() });
    if (typeof b.width === "number") extra.set("w", String(Math.floor(b.width)));
    return c.json({ url: `${publicBase(c)}/player/heatmap?${link.query}&${extra}`, expires_at: link.expires_at });
  });

  app.route("/", api);
  return app;
}
