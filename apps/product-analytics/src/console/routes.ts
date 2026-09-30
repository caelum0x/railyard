// The console API. Every call carries a 5-minute org token; the project in
// the path must be one of the token's app ids and belong to its org
// (checked against the site directory). Queries run as the request role
// with the project set, and bind project_id.

import { Hono, type Context } from "hono";
import { capMessage } from "../caps.ts";
import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { exportPerson } from "../gdpr/export.ts";
import type { GdprHooks } from "../gdpr/hooks.ts";
import { listJobs, requestDelete } from "../gdpr/jobs.ts";
import { personRef } from "../gdpr/person-ref.ts";
import type { Logger } from "../log.ts";
import { GDPR_DELETE_WITHIN_MS, RETENTION_DAYS, retentionDaysFor } from "../plans.ts";
import { verifyConsoleToken, type ConsoleClaims } from "../tenancy/console-token.ts";
import { getSettings, mergeSettings, putSettings, SettingsError } from "../tenancy/settings.ts";
import type { Site, SiteDirectory } from "../tenancy/site-directory.ts";

export interface ConsoleDeps {
  db: Db;
  sites: SiteDirectory;
  secret: string | null;
  hooks: GdprHooks;
  log: Logger;
  now: () => Date;
}

type Env = { Variables: { claims: ConsoleClaims; site: Site } };

const DISTINCT = (v: string) => v.length > 0 && v.length <= 200;

export function consoleRoutes(deps: ConsoleDeps): Hono<Env> {
  const app = new Hono<Env>();

  app.use("/v1/projects/:id/*", async (c, next) => {
    if (!deps.secret) return c.json({ error: "The console connection is not configured on this service." }, 503);
    const auth = c.req.header("authorization") ?? "";
    const claims = await verifyConsoleToken(deps.secret, auth.startsWith("Bearer ") ? auth.slice(7) : null, deps.now());
    if (!claims) return c.json({ error: "Sign in again." }, 401);
    const id = c.req.param("id");
    if (!isUuid(id) || !claims.app_ids.includes(id)) return c.json({ error: "This app is not in your organization." }, 403);
    const site = await deps.sites.byProject(id);
    if (!site || site.org_id !== claims.org_id) return c.json({ error: "This app is not in your organization." }, 403);
    c.set("claims", claims);
    c.set("site", site);
    await next();
  });

  const canWrite = (c: Context<Env>) => c.get("claims").role !== "viewer";

  app.get("/v1/projects/:id/status", async (c) => {
    const site = c.get("site");
    const now = deps.now();
    const month = `${now.toISOString().slice(0, 7)}-01`;
    const [first, usage, orgUsed] = await Promise.all([
      deps.db.asProject(site.project_id, async (q) => (await q.query("select event_uuid, event, at from pa.project_firsts where project_id = $1", [site.project_id])).rows[0] ?? null),
      deps.db.asProject(site.project_id, async (q) => (await q.query("select received, stored, capped, dropped from pa.usage_monthly where project_id = $1 and month = $2", [site.project_id, month])).rows[0] ?? null),
      deps.db.query<{ n: string }>("select coalesce(sum(stored), 0)::bigint as n from pa.usage_monthly where org_id = $1 and month = $2", [site.org_id, month]),
    ]);
    const used = Number(orgUsed[0]?.n ?? 0);
    const settings = await getSettings(deps.db, site.project_id);
    return c.json({
      project: { id: site.project_id, name: site.name },
      paused: site.paused,
      first_event: first ? { uuid: first.event_uuid, event: first.event, at: new Date(first.at).toISOString() } : null,
      usage: {
        month,
        received: Number(usage?.received ?? 0),
        stored: Number(usage?.stored ?? 0),
        capped: Number(usage?.capped ?? 0),
        dropped: Number(usage?.dropped ?? 0),
      },
      org_stored_this_month: used,
      events_limit: site.events_limit,
      cap_message: capMessage(used, site.events_limit, now),
      retention_days: retentionDaysFor(site.plan, settings.retention_days),
    });
  });

  app.get("/v1/projects/:id/events", async (c) => {
    const site = c.get("site");
    const limit = Math.min(200, Math.max(1, Number(c.req.query("limit") ?? "50") || 50));
    const since = new Date(deps.now().getTime() - 31 * 86_400_000).toISOString();
    const rows = await deps.db.asProject(site.project_id, async (q) =>
      (
        await q.query(
          `select e.uuid, e.event, e.distinct_id, e."timestamp", e.inserted_at, e.properties ->> '$current_url' as url,
                  e.properties ->> '$screen_name' as screen, e.properties ->> '$lib' as lib
             from pa.events e where e.project_id = $1 and e."timestamp" >= $2
            order by e."timestamp" desc limit $3`,
          [site.project_id, since, limit],
        )
      ).rows,
    );
    return c.json({ events: rows });
  });

  app.get("/v1/projects/:id/persons", async (c) => {
    const site = c.get("site");
    const search = (c.req.query("search") ?? "").trim().slice(0, 200);
    const limit = Math.min(100, Math.max(1, Number(c.req.query("limit") ?? "50") || 50));
    const rows = await deps.db.asProject(site.project_id, async (q) =>
      (
        await q.query(
          `select p.id, p.properties, p.is_identified, p.created_at,
                  (select array_agg(d.distinct_id order by d.distinct_id) from pa.person_distinct_ids d where d.project_id = p.project_id and d.person_id = p.id) as distinct_ids
             from pa.persons p
            where p.project_id = $1
              and ($2 = '' or exists (select 1 from pa.person_distinct_ids d where d.project_id = p.project_id and d.person_id = p.id and d.distinct_id ilike $3)
                   or p.properties ->> 'email' ilike $3 or p.properties ->> 'name' ilike $3)
            order by p.created_at desc limit $4`,
          [site.project_id, search, `%${search.replace(/[\\%_]/g, (m) => `\\${m}`)}%`, limit],
        )
      ).rows,
    );
    return c.json({ persons: rows });
  });

  app.get("/v1/projects/:id/persons/:distinct_id", async (c) => {
    const site = c.get("site");
    const distinctId = c.req.param("distinct_id");
    if (!DISTINCT(distinctId)) return c.json({ error: "Name one person." }, 400);
    const out = await deps.db.asProject(site.project_id, async (q) => {
      const { person, ref } = await personRef(q, deps.db, site.project_id, distinctId, deps.now());
      if (!person) return null;
      const events = await q.query(
        `select uuid, event, distinct_id, "timestamp", properties ->> '$current_url' as url
           from pa.events where project_id = $1 and person_id = any($2::uuid[]) order by "timestamp" desc limit 100`,
        [site.project_id, ref.personIds],
      );
      const count = await q.query<{ n: string }>("select count(*)::bigint as n from pa.events where project_id = $1 and person_id = any($2::uuid[])", [site.project_id, ref.personIds]);
      return { person, distinct_ids: ref.distinctIds, events: events.rows, event_count: Number(count.rows[0].n) };
    });
    return out ? c.json(out) : c.json({ error: "No person has that id in this app." }, 404);
  });

  app.post("/v1/projects/:id/persons/:distinct_id/delete", async (c) => {
    if (!canWrite(c)) return c.json({ error: "Your role can view this app but not change it." }, 403);
    const site = c.get("site");
    const distinctId = c.req.param("distinct_id");
    if (!DISTINCT(distinctId)) return c.json({ error: "Name one person." }, 400);
    const job = await requestDelete(deps.db, site.project_id, distinctId, `console:${c.get("claims").org_id}`, deps.now());
    return c.json({ job, within_ms: GDPR_DELETE_WITHIN_MS }, 202);
  });

  app.get("/v1/projects/:id/persons/:distinct_id/export", async (c) => {
    const site = c.get("site");
    const distinctId = c.req.param("distinct_id");
    if (!DISTINCT(distinctId)) return c.json({ error: "Name one person." }, 400);
    const doc = await exportPerson(deps.db, deps.hooks, site.project_id, distinctId, deps.now());
    if (!doc) return c.json({ error: "No person has that id in this app." }, 404);
    return c.body(JSON.stringify(doc, null, 2), 200, {
      "content-type": "application/json",
      "content-disposition": `attachment; filename="person-export.json"`,
      "cache-control": "no-store",
    });
  });

  app.get("/v1/projects/:id/privacy/jobs", async (c) => {
    return c.json({ jobs: await listJobs(deps.db, c.get("site").project_id) });
  });

  app.get("/v1/projects/:id/settings", async (c) => c.json({ settings: await getSettings(deps.db, c.get("site").project_id) }));

  app.put("/v1/projects/:id/settings", async (c) => {
    if (!canWrite(c)) return c.json({ error: "Your role can view this app but not change it." }, 403);
    const site = c.get("site");
    try {
      const next = mergeSettings(await getSettings(deps.db, site.project_id), await c.req.json().catch(() => null));
      return c.json({ settings: await putSettings(deps.db, site, next) });
    } catch (err) {
      if (err instanceof SettingsError) return c.json({ error: err.message }, 400);
      throw err;
    }
  });

  app.get("/v1/projects/:id/groups", async (c) => {
    const site = c.get("site");
    const out = await deps.db.asProject(site.project_id, async (q) => ({
      types: (await q.query("select group_type, group_type_index from pa.group_types where project_id = $1 order by group_type_index", [site.project_id])).rows,
      groups: (await q.query("select group_type_index, group_key, properties, updated_at from pa.groups where project_id = $1 order by updated_at desc limit 100", [site.project_id])).rows,
    }));
    return c.json(out);
  });

  app.get("/v1/plans", (c) => c.json({ retention_days: RETENTION_DAYS, delete_within_ms: GDPR_DELETE_WITHIN_MS }));

  return app;
}
