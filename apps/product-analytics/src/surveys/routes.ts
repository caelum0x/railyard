// Surveys (A3; handed over by A1).
//   SDK:     GET /api/surveys/?token=   running surveys in the SDKs' shape (public, like the token)
//   Console: /v1/projects/:id/surveys (list, create), /surveys/:sid (read, edit, delete),
//            /surveys/:sid/status {running|stopped}, /surveys/:sid/results
// Answers arrive as `survey sent` events through the normal capture path.

import { Hono, type Context } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { CORS_HEADERS } from "../capture/routes.ts";
import { TOKEN } from "../tenancy/site-directory.ts";
import { ApiError, consoleGuard, jsonBody, plainErrors, projectOf, requireWriter, uuidParam, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { surveyResults } from "./results.ts";
import { activeSurveys, checkSurvey, createSurvey, deleteSurvey, getSurvey, listSurveys, MAX_RUNNING, setSurveyStatus, updateSurvey } from "./store.ts";

export function surveysRoutes(deps: ServiceDeps): Hono {
  const app = new Hono();

  const publicList = async (c: Context) => {
    const origin = c.req.header("origin");
    const url = new URL(c.req.url);
    const token = url.searchParams.get("token") ?? url.searchParams.get("api_key") ?? "";
    if (!TOKEN.test(token)) return c.json({ type: "authentication_error", detail: "The project token is missing." }, 401, CORS_HEADERS(origin));
    try {
      const site = await deps.sites.byToken(token);
      if (!site) return c.json({ type: "authentication_error", detail: "The project token is not valid." }, 401, CORS_HEADERS(origin));
      if (site.paused) return c.json({ surveys: [] }, 200, CORS_HEADERS(origin));
      return c.json({ surveys: await activeSurveys(deps.db, site.project_id) }, 200, { ...CORS_HEADERS(origin), "cache-control": "public, max-age=30" });
    } catch (err) {
      deps.log.error("surveys read failed", { error: (err as Error).message });
      return c.json({ type: "server_error", detail: "Try again shortly." }, 503, CORS_HEADERS(origin));
    }
  };
  for (const p of ["/api/surveys", "/api/surveys/"]) {
    app.options(p, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
    app.get(p, publicList);
  }

  const api = new Hono<ConsoleEnv>();
  const P = "/v1/projects/:id/surveys";
  const guard = consoleGuard(deps);
  api.use(P, guard);
  api.use(`${P}/*`, guard);
  plainErrors(api, deps.log, "surveys");

  api.get(P, async (c) => c.json({ surveys: await listSurveys(deps.db, projectOf(c)), max_running: MAX_RUNNING }));
  api.post(P, async (c) => {
    requireWriter(c);
    return c.json({ survey: await createSurvey(deps.db, projectOf(c), checkSurvey(await jsonBody(c)), deps.now()) }, 201);
  });
  api.get(`${P}/:sid`, async (c) => {
    const id = uuidParam(c, "sid", "survey");
    return c.json({ survey: await deps.db.asProject(projectOf(c), (tx) => getSurvey(tx, projectOf(c), id)) });
  });
  api.put(`${P}/:sid`, async (c) => {
    requireWriter(c);
    return c.json({ survey: await updateSurvey(deps.db, projectOf(c), uuidParam(c, "sid", "survey"), await jsonBody(c), deps.now()) });
  });
  api.delete(`${P}/:sid`, async (c) => {
    requireWriter(c);
    await deleteSurvey(deps.db, projectOf(c), uuidParam(c, "sid", "survey"));
    return c.json({ deleted: true });
  });
  api.post(`${P}/:sid/status`, async (c) => {
    requireWriter(c);
    const b = await jsonBody(c);
    if (b.status !== "running" && b.status !== "stopped") throw new ApiError(400, "The status must be running or stopped.");
    return c.json({ survey: await setSurveyStatus(deps.db, projectOf(c), uuidParam(c, "sid", "survey"), b.status, deps.now()) });
  });
  api.get(`${P}/:sid/results`, async (c) => {
    const id = uuidParam(c, "sid", "survey");
    const survey = await deps.db.asProject(projectOf(c), (tx) => getSurvey(tx, projectOf(c), id));
    return c.json({ results: await surveyResults(deps.db, projectOf(c), survey) });
  });

  app.route("/", api);
  return app;
}
