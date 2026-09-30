import { Hono, type Context } from "hono";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { getSettings } from "../tenancy/settings.ts";
import { TOKEN, type SiteDirectory } from "../tenancy/site-directory.ts";
import { assembleConfig, configScript, type ConfigProvider } from "./registry.ts";

export function remoteConfigRoutes(deps: { db: Db; sites: SiteDirectory; log: Logger; sections: ConfigProvider[] }): Hono {
  const app = new Hono();
  const load = async (c: Context, token: string) => {
    if (!TOKEN.test(token)) return null;
    const site = await deps.sites.byToken(token);
    if (!site) return null;
    return assembleConfig({ site, settings: await getSettings(deps.db, site.project_id) }, deps.sections);
  };
  const cors = { "access-control-allow-origin": "*", "cache-control": "public, max-age=60" };
  app.get("/array/:token/config", async (c) => {
    try {
      const cfg = await load(c, c.req.param("token"));
      return cfg ? c.json(cfg, 200, cors) : c.json({ type: "not_found", detail: "Unknown project token." }, 404, cors);
    } catch (err) {
      deps.log.error("remote config failed", { error: (err as Error).message });
      return c.json({ type: "server_error", detail: "Try again shortly." }, 503, cors);
    }
  });
  app.get("/array/:token/config.js", async (c) => {
    const token = c.req.param("token");
    try {
      const cfg = await load(c, token);
      if (!cfg) return c.text("// unknown project token", 404, cors);
      return c.body(configScript(token, cfg), 200, { ...cors, "content-type": "application/javascript" });
    } catch (err) {
      deps.log.error("remote config failed", { error: (err as Error).message });
      return c.text("// try again shortly", 503, cors);
    }
  });
  return app;
}
