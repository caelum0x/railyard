// Alerts, dashboard subscriptions and unsubscribe (QY1.4, A25).
//   Console (org token): /v1/projects/:id/alerts            GET list, POST create
//                        /v1/projects/:id/alerts/:aid       PUT {enabled?, recipients?, check_now?}, DELETE
//                        /v1/projects/:id/alerts/:aid/log   the mail log
//                        /v1/projects/:id/subscriptions     GET list, POST create
//                        /v1/projects/:id/subscriptions/:sid  PUT {enabled?, recipients?, send_now?}, DELETE
//                        /v1/projects/:id/subscriptions/:sid/log
//   Public:              /v1/unsubscribe?p&e&t              GET (a page) or POST (one-click)
// Checks and sends run on the service scheduler (alerts every minute, each
// alert at its own hour or day; subscriptions at their weekly slot).

import { Hono, type Context } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { isUuid } from "../db.ts";
import { consoleGuard, jsonBody, plainErrors, projectOf, requireWriter, uuidParam, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { createAlert, deleteAlert, listAlerts, parseAlert, updateAlert, MAX_ALERTS } from "./alerts.ts";
import { EMAIL, escapeHtml } from "./mail.ts";
import { checkUnsubscribeToken, mailLog, unsubscribe } from "./send.ts";
import { createSubscription, deleteSubscription, listSubscriptions, MAX_SUBSCRIPTIONS, updateSubscription, WEEKDAYS } from "./subscriptions.ts";

const page = (title: string, body: string) =>
  `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>${escapeHtml(title)}</title></head><body style="font-family:system-ui,sans-serif;max-width:480px;margin:48px auto;padding:0 16px"><h1 style="font-size:20px">${escapeHtml(title)}</h1><p>${escapeHtml(body)}</p></body></html>`;

export function alertsRoutes(deps: ServiceDeps): Hono {
  const app = new Hono();

  const unsub = async (c: Context) => {
    const q = new URL(c.req.url).searchParams;
    const p = q.get("p") ?? "";
    const e = (q.get("e") ?? "").toLowerCase();
    const t = q.get("t") ?? "";
    const headers = { "cache-control": "no-store", "referrer-policy": "no-referrer", "x-frame-options": "DENY" };
    if (!isUuid(p) || !EMAIL.test(e) || !checkUnsubscribeToken(deps.consoleSecret, p, e, t)) {
      return c.html(page("Link not valid", "This unsubscribe link is not valid. Use the link in the latest email."), 400, headers);
    }
    await unsubscribe(deps.db, p, e);
    return c.html(page("Unsubscribed", `${e} will get no more alert or report emails from this app.`), 200, headers);
  };
  app.get("/v1/unsubscribe", unsub);
  app.post("/v1/unsubscribe", unsub);

  const api = new Hono<ConsoleEnv>();
  const guard = consoleGuard(deps);
  for (const p of ["/v1/projects/:id/alerts", "/v1/projects/:id/alerts/*", "/v1/projects/:id/subscriptions", "/v1/projects/:id/subscriptions/*"]) api.use(p, guard);
  plainErrors(api, deps.log, "alerts");

  api.get("/v1/projects/:id/alerts", async (c) => c.json({ alerts: await listAlerts(deps.db, projectOf(c)), max_alerts: MAX_ALERTS, mail_configured: deps.mailer.configured }));
  api.post("/v1/projects/:id/alerts", async (c) => {
    requireWriter(c);
    return c.json({ alert: await createAlert(deps.db, projectOf(c), parseAlert(await jsonBody(c))) }, 201);
  });
  api.put("/v1/projects/:id/alerts/:aid", async (c) => {
    requireWriter(c);
    return c.json({ alert: await updateAlert(deps.db, projectOf(c), uuidParam(c, "aid", "alert"), await jsonBody(c)) });
  });
  api.delete("/v1/projects/:id/alerts/:aid", async (c) => {
    requireWriter(c);
    await deleteAlert(deps.db, projectOf(c), uuidParam(c, "aid", "alert"));
    return c.json({ deleted: true });
  });
  api.get("/v1/projects/:id/alerts/:aid/log", async (c) => c.json({ log: await mailLog(deps.db, projectOf(c), "alert", uuidParam(c, "aid", "alert")) }));

  api.get("/v1/projects/:id/subscriptions", async (c) =>
    c.json({ subscriptions: await listSubscriptions(deps.db, projectOf(c)), weekdays: WEEKDAYS, max_subscriptions: MAX_SUBSCRIPTIONS, mail_configured: deps.mailer.configured }),
  );
  api.post("/v1/projects/:id/subscriptions", async (c) => {
    requireWriter(c);
    return c.json({ subscription: await createSubscription(deps.db, projectOf(c), await jsonBody(c), deps.now()) }, 201);
  });
  api.put("/v1/projects/:id/subscriptions/:sid", async (c) => {
    requireWriter(c);
    return c.json({ subscription: await updateSubscription(deps.db, projectOf(c), uuidParam(c, "sid", "subscription"), await jsonBody(c), deps.now()) });
  });
  api.delete("/v1/projects/:id/subscriptions/:sid", async (c) => {
    requireWriter(c);
    await deleteSubscription(deps.db, projectOf(c), uuidParam(c, "sid", "subscription"));
    return c.json({ deleted: true });
  });
  api.get("/v1/projects/:id/subscriptions/:sid/log", async (c) => c.json({ log: await mailLog(deps.db, projectOf(c), "subscription", uuidParam(c, "sid", "subscription")) }));

  app.route("/", api);
  return app;
}
