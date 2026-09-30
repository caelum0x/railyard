// Recordings (A3; handed over by A1).
//   SDK:     /s/, /i/v0/s/           recording ingest ($snapshot batches, any capture encoding)
//            /static/<name>.js       the SDK's lazily loaded recorder and survey code
//   Console: /v1/projects/:id/recording-settings, /replays, /replays/for-people,
//            /replays/:session, /replays/:session/link (a signed 10-minute player URL)
//   Link:    /player/replay?r&p&exp&sig  the player page, and /player/replay/events its lines
//            /player/assets/*            the pages' static files

import { Hono, type Context } from "hono";
import { stream } from "hono/streaming";
import type { ServiceDeps } from "../app-deps.ts";
import { CORS_HEADERS, problem, readPayload } from "../capture/routes.ts";
import { EventError } from "../capture/normalize.ts";
import { isUuid } from "../db.ts";
import { bindSections } from "../remote-config/sections.ts";
import { ReplayIngest } from "./ingest.ts";
import { replayRetentionDays } from "./limits.ts";
import { replayStore } from "./object-store.ts";
import { assetResponse, pageHeaders, pageHtml, sdkResponse } from "./player-pages.ts";
import { deleteReplays, identitiesOf, identitiesOfPeople, latestReplays, MAX_LIST, replayBySession, replayForEvent, replayLines, replaysOfIdentities } from "./query.ts";
import { getRecordingSettings, mergeRecordingSettings, putRecordingSettings } from "./settings.ts";
import { ApiError, consoleGuard, jsonBody, plainErrors, projectOf, requireWriter, type ConsoleEnv } from "./shared/console-guard.ts";
import { signLink, verifyLink } from "./shared/signed-link.ts";
import { extractSnapshots, SnapshotError } from "./snapshot.ts";

export const SESSION_PARAM = /^[A-Za-z0-9._:-]{1,200}$/;

/** Where links point: PA_PUBLIC_URL, else the origin the console called. */
export function publicBase(c: Context): string {
  const env = process.env.PA_PUBLIC_URL?.trim();
  return env ? env.replace(/\/+$/, "") : new URL(c.req.url).origin;
}

export function replayRoutes(deps: ServiceDeps): Hono {
  const app = new Hono();
  bindSections(deps.db);

  // SDK: recording ingest.
  const ingest = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      let batches;
      try {
        batches = extractSnapshots(payload);
      } catch (err) {
        if (err instanceof SnapshotError) throw new EventError(400, `The recording could not be read: ${err.message}.`);
        throw err;
      }
      if (batches.length === 0) return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
      const token = batches[0].token ?? new URL(c.req.url).searchParams.get("token");
      if (!token) throw new EventError(401, "The project token is missing.");
      if (batches.some((b) => b.token && b.token !== token)) throw new EventError(400, "Every recording of one request must use the same project token.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const store = replayStore();
      // Recording off, no store, or a paused org: a normal answer (so the SDK does not retry), nothing kept.
      if (!store || site.paused || !(await getRecordingSettings(deps.db, site.project_id)).replay_enabled) return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
      await new ReplayIngest(deps.db, store, deps.log, deps.now).ingest(site, batches);
      return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };
  for (const p of ["/s", "/s/", "/i/v0/s", "/i/v0/s/"]) {
    app.options(p, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
    app.post(p, ingest);
    app.get(p, ingest);
  }

  app.get("/static/:name", (c) => sdkResponse(c, c.req.param("name")));
  app.get("/static/:version/:name", (c) => sdkResponse(c, c.req.param("name")));
  app.get("/player/assets/*", (c) => assetResponse(c, new URL(c.req.url).pathname.replace(/^\/player\/assets\//, "")));

  // Signed player page and its lines.
  const linked = (c: Context) => {
    if (!deps.consoleSecret) return null;
    const v = verifyLink(deps.consoleSecret, new URL(c.req.url).searchParams, deps.now());
    if (!v || !v.resource.startsWith("replay:")) return null;
    return { project: v.project, session: decodeURIComponent(v.resource.slice("replay:".length)) };
  };
  const expired = (c: Context) =>
    c.html(pageHtml({ title: "Link expired", scripts: [], styles: ["player.css"], data: {} }).replace("Loading...", "This link has expired or is not valid. Open the recording again from the console."), 403, pageHeaders());

  app.get("/player/replay", async (c) => {
    const l = linked(c);
    if (!l) return expired(c);
    const q = new URL(c.req.url).searchParams;
    const t = Number(q.get("t") ?? "0");
    const events = `/player/replay/events?${new URLSearchParams({ r: q.get("r")!, p: q.get("p")!, exp: q.get("exp")!, sig: q.get("sig")! })}`;
    return c.body(
      pageHtml({ title: "Recording", scripts: ["vendor/rrweb-player/rrweb-player.min.js", "replay.js"], styles: ["vendor/rrweb-player/style.css", "player.css"], data: { events, offset: String(Number.isFinite(t) && t > 0 ? Math.floor(t) : 0) } }),
      200,
      pageHeaders(),
    );
  });

  app.get("/player/replay/events", async (c) => {
    const l = linked(c);
    if (!l) return c.json({ error: "This link has expired or is not valid." }, 403);
    const store = replayStore();
    if (!store) return c.json({ error: "Recordings are not stored on this deployment." }, 503);
    const exists = await deps.db.asProject(l.project, (tx) => replayBySession(tx, l.project, l.session));
    if (!exists) return c.json({ error: "This recording no longer exists." }, 404);
    c.header("content-type", "application/x-ndjson; charset=utf-8");
    c.header("cache-control", "no-store");
    c.header("referrer-policy", "no-referrer");
    return stream(c, async (s) => {
      for await (const text of replayLines(deps.db, store, l.project, l.session)) await s.write(text);
    });
  });

  // Console API.
  const P = "/v1/projects/:id";
  const api = new Hono<ConsoleEnv>();
  const guard = consoleGuard(deps);
  api.use(`${P}/replays`, guard);
  api.use(`${P}/replays/*`, guard);
  api.use(`${P}/recording-settings`, guard);
  plainErrors(api, deps.log, "replays");

  api.get(`${P}/recording-settings`, async (c) =>
    c.json({ settings: await getRecordingSettings(deps.db, projectOf(c)), store: replayStore()?.kind ?? null, retention_days: replayRetentionDays(c.get("site")) }),
  );
  api.put(`${P}/recording-settings`, async (c) => {
    requireWriter(c);
    const next = mergeRecordingSettings(await getRecordingSettings(deps.db, projectOf(c)), await jsonBody(c));
    if (next.replay_enabled && !replayStore()) throw new ApiError(409, "Recordings need object storage, which is not set up on this deployment yet.");
    return c.json({ settings: await putRecordingSettings(deps.db, c.get("site"), next) });
  });

  api.get(`${P}/replays`, async (c) => {
    const pid = projectOf(c);
    const q = new URL(c.req.url).searchParams;
    const limit = Math.min(MAX_LIST, Math.max(1, Number(q.get("limit") ?? "50") || 50));
    const event = q.get("event");
    const distinct = q.get("distinct_id");
    const person = q.get("person_id");
    return deps.db.asProject(pid, async (tx) => {
      if (event) {
        const hit = await replayForEvent(tx, pid, event);
        return c.json({ replays: hit ? [hit.replay] : [], offset_ms: hit?.offset_ms ?? null });
      }
      if (distinct) return c.json({ replays: await replaysOfIdentities(tx, pid, await identitiesOf(tx, pid, distinct.slice(0, 200)), limit) });
      if (person) {
        if (!isUuid(person)) throw new ApiError(400, "The person id must be a uuid.");
        return c.json({ replays: await replaysOfIdentities(tx, pid, await identitiesOfPeople(tx, pid, [person]), limit) });
      }
      const before = q.get("before");
      const b = before ? new Date(before) : null;
      if (b && Number.isNaN(b.getTime())) throw new ApiError(400, "before must be a date.");
      return c.json({ replays: await latestReplays(tx, pid, b, limit) });
    });
  });

  api.post(`${P}/replays/for-people`, async (c) => {
    const pid = projectOf(c);
    const b = await jsonBody(c);
    const people = Array.isArray(b.person_ids) ? b.person_ids.filter(isUuid) : [];
    const distinct = Array.isArray(b.distinct_ids) ? b.distinct_ids.filter((d): d is string => typeof d === "string" && d.length > 0 && d.length <= 200) : [];
    if (people.length + distinct.length === 0 || people.length + distinct.length > 1000) throw new ApiError(400, "Send 1 to 1000 person_ids or distinct_ids.");
    return deps.db.asProject(pid, async (tx) => {
      const who = await identitiesOfPeople(tx, pid, people);
      return c.json({ replays: await replaysOfIdentities(tx, pid, { distinctIds: [...who.distinctIds, ...distinct], personIds: who.personIds }, MAX_LIST) });
    });
  });

  const sessionOf = (c: Context) => {
    const s = c.req.param("session") ?? "";
    if (!SESSION_PARAM.test(s)) throw new ApiError(404, "That recording does not exist.");
    return s;
  };

  api.get(`${P}/replays/:session`, async (c) => {
    const pid = projectOf(c);
    const r = await deps.db.asProject(pid, (tx) => replayBySession(tx, pid, sessionOf(c)));
    if (!r) throw new ApiError(404, "That recording does not exist.");
    return c.json({ replay: r });
  });

  api.post(`${P}/replays/:session/link`, async (c) => {
    const pid = projectOf(c);
    const session = sessionOf(c);
    const r = await deps.db.asProject(pid, (tx) => replayBySession(tx, pid, session));
    if (!r) throw new ApiError(404, "That recording does not exist.");
    const b = await jsonBody(c);
    const t = typeof b.offset_ms === "number" && Number.isFinite(b.offset_ms) && b.offset_ms > 0 ? Math.floor(b.offset_ms) : 0;
    const link = signLink(deps.consoleSecret!, `replay:${encodeURIComponent(session)}`, pid, deps.now());
    return c.json({ url: `${publicBase(c)}/player/replay?${link.query}${t ? `&t=${t}` : ""}`, expires_at: link.expires_at });
  });

  api.delete(`${P}/replays/:session`, async (c) => {
    requireWriter(c);
    const n = await deleteReplays(deps.db, replayStore(), projectOf(c), [sessionOf(c)]);
    if (!n) throw new ApiError(404, "That recording does not exist.");
    return c.json({ deleted: n });
  });

  app.route("/", api);
  return app;
}
