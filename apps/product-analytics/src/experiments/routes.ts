// Experiments on the flags slot, under /v1/projects/:id/experiments (console
// token). The service never writes a flag. Starting or stopping is:
//   1. GET  .../experiments/:eid/flag-value?for=start|stop[&variant=]  -> the flag change to make
//   2. the console calls the flags slot's own setFlag with that value
//   3. POST .../experiments/:eid/status {status}                        -> recorded
// The detail answer reads the flag back through the slot's public client API
// (flag_state: live, waiting or missing), so a skipped step 2 shows.

import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { getSettings } from "../tenancy/settings.ts";
import { ApiError, consoleGuard, jsonBody, plainErrors, projectOf, requireWriter, uuidParam, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { buildFlagValue, parseFlagValue } from "./flag-value.ts";
import { experimentResults } from "./results.ts";
import { checkInput, createExperiment, deleteExperiment, getExperiment, listExperiments, setStatus, updateExperiment, type Experiment } from "./store.ts";

export type FlagState = "live" | "waiting" | "missing" | "not_connected";

/** Whether the slot's flag carries what this experiment expects now. */
export async function flagState(deps: Pick<ServiceDeps, "db" | "flags">, projectId: string, exp: Experiment): Promise<FlagState> {
  const settings = await getSettings(deps.db, projectId);
  if (!settings.flags_client_key) return "not_connected";
  let flags;
  try {
    flags = await deps.flags.read(settings.flags_client_key);
  } catch {
    return "not_connected";
  }
  const f = flags[exp.flag_key];
  if (!f) return "missing";
  const v = parseFlagValue(f.value);
  if (exp.status === "draft") return v?.experiment === exp.id ? "live" : "missing";
  return f.enabled && v?.experiment === exp.id ? "live" : "waiting";
}

export function flagChange(exp: Experiment, action: string | undefined, variant: string | undefined): { key: string; enabled: boolean; value: string } {
  if (action === "start") {
    if (exp.status !== "draft") throw new ApiError(409, "Only a draft experiment can start.");
    return { key: exp.flag_key, enabled: true, value: buildFlagValue({ rollout: exp.rollout, variants: exp.variants, experiment: exp.id }) };
  }
  if (action === "stop") {
    if (exp.status !== "running") throw new ApiError(409, "Only a running experiment can stop.");
    const keep = variant ?? "control";
    if (!exp.variants.some((v) => v.key === keep)) throw new ApiError(400, "Choose one of the experiment's variants to keep.");
    // Everyone gets the kept variant from now on; apps keep reading the same flag.
    return { key: exp.flag_key, enabled: true, value: buildFlagValue({ rollout: 100, variants: [{ key: keep, weight: 100 }], experiment: exp.id }) };
  }
  throw new ApiError(400, "Ask for the flag change to start or to stop.");
}

export function experimentsRoutes(deps: ServiceDeps): Hono<ConsoleEnv> {
  const app = new Hono<ConsoleEnv>();
  const P = "/v1/projects/:id/experiments";
  const guard = consoleGuard(deps);
  app.use(P, guard);
  app.use(`${P}/*`, guard);
  plainErrors(app, deps.log, "experiments");

  app.get(P, async (c) => c.json({ experiments: await listExperiments(deps.db, projectOf(c)) }));

  app.post(P, async (c) => {
    requireWriter(c);
    const input = checkInput(await jsonBody(c));
    return c.json({ experiment: await createExperiment(deps.db, projectOf(c), input, deps.now()) }, 201);
  });

  app.get(`${P}/:eid`, async (c) => {
    const id = uuidParam(c, "eid", "experiment");
    const exp = await deps.db.asProject(projectOf(c), (tx) => getExperiment(tx, projectOf(c), id));
    return c.json({ experiment: exp, flag_state: await flagState(deps, projectOf(c), exp) });
  });

  app.put(`${P}/:eid`, async (c) => {
    requireWriter(c);
    return c.json({ experiment: await updateExperiment(deps.db, projectOf(c), uuidParam(c, "eid", "experiment"), await jsonBody(c), deps.now()) });
  });

  app.delete(`${P}/:eid`, async (c) => {
    requireWriter(c);
    await deleteExperiment(deps.db, projectOf(c), uuidParam(c, "eid", "experiment"));
    return c.json({ deleted: true });
  });

  app.get(`${P}/:eid/flag-value`, async (c) => {
    requireWriter(c);
    const id = uuidParam(c, "eid", "experiment");
    const exp = await deps.db.asProject(projectOf(c), (tx) => getExperiment(tx, projectOf(c), id));
    return c.json({ flags_env: exp.flags_env, flag: flagChange(exp, c.req.query("for"), c.req.query("variant")) });
  });

  app.post(`${P}/:eid/status`, async (c) => {
    requireWriter(c);
    const b = await jsonBody(c);
    if (b.status !== "running" && b.status !== "stopped") throw new ApiError(400, "The status must be running or stopped.");
    return c.json({ experiment: await setStatus(deps.db, projectOf(c), uuidParam(c, "eid", "experiment"), b.status, deps.now()) });
  });

  app.get(`${P}/:eid/results`, async (c) => {
    const id = uuidParam(c, "eid", "experiment");
    const exp = await deps.db.asProject(projectOf(c), (tx) => getExperiment(tx, projectOf(c), id));
    return c.json({ results: await experimentResults(deps.db, projectOf(c), exp, deps.now()) });
  });

  return app;
}
