// Alerts on saved insights (QY1.4, A25). A threshold alert fires when the
// watched value (metric.ts) is above or below a number; an anomaly alert when
// it is `sensitivity` standard deviations from the earlier intervals. An
// alert mails its recipients once when it starts firing and again only after
// it has cleared. Each check is leased, so two scheduler passes never send
// the same mail twice; every statement binds the project id.

import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { computeCached } from "../insights/cache.ts";
import { getSavedInsight } from "../insights/store.ts";
import { parseQuery } from "../insights/validate.ts";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { EMAIL, escapeHtml } from "./mail.ts";
import { headline, zScore } from "./metric.ts";
import { sendAll, type SendDeps } from "./send.ts";

export const MAX_ALERTS = 50;
export const MAX_RECIPIENTS = 20;
export const LEASE_MS = 5 * 60_000;
const EVERY_MS = { hour: 3600_000, day: 86_400_000 } as const;

export interface Alert {
  id: string;
  project_id: string;
  insight_id: string;
  name: string;
  kind: "threshold" | "anomaly";
  series_index: number;
  direction: "above" | "below" | "either";
  threshold: number | null;
  sensitivity: number | null;
  check_every: "hour" | "day";
  recipients: string[];
  enabled: boolean;
  state: "ok" | "firing" | "error";
  last_value: number | null;
  last_checked_at: string | null;
  last_fired_at: string | null;
  last_error: string | null;
  next_check_at: string | null;
  created_at: string;
}

const COLS =
  "id, project_id, insight_id, name, kind, series_index, direction, threshold, sensitivity, check_every, recipients, enabled, state, last_value, last_checked_at, last_fired_at, last_error, next_check_at, created_at";
const iso = (v: unknown) => (v instanceof Date ? v.toISOString() : ((v as string | null) ?? null));
const toAlert = (r: Record<string, unknown>): Alert => ({
  ...(r as unknown as Alert),
  last_checked_at: iso(r.last_checked_at),
  last_fired_at: iso(r.last_fired_at),
  next_check_at: iso(r.next_check_at),
  created_at: iso(r.created_at)!,
});

export function checkRecipients(v: unknown): string[] {
  if (!Array.isArray(v) || v.length < 1 || v.length > MAX_RECIPIENTS) throw new ApiError(400, `Name 1 to ${MAX_RECIPIENTS} email addresses.`);
  const out = [...new Set(v.map((x) => (typeof x === "string" ? x.trim().toLowerCase() : "")))];
  for (const e of out) if (!EMAIL.test(e)) throw new ApiError(400, `${e || "An empty address"} is not an email address.`);
  return out;
}

export interface NewAlert {
  insight_id: string;
  name: string;
  kind: "threshold" | "anomaly";
  series_index: number;
  direction: "above" | "below" | "either";
  threshold: number | null;
  sensitivity: number | null;
  check_every: "hour" | "day";
  recipients: string[];
}

export function parseAlert(b: Record<string, unknown>): NewAlert {
  const name = text(b.name, "Name", 200)!;
  if (typeof b.insight_id !== "string" || !isUuid(b.insight_id)) throw new ApiError(400, "Pick a saved insight.");
  const kind = b.kind;
  if (kind !== "threshold" && kind !== "anomaly") throw new ApiError(400, "The alert is a threshold or an anomaly alert.");
  const series = b.series_index ?? 0;
  if (typeof series !== "number" || !Number.isInteger(series) || series < 0 || series > 19) throw new ApiError(400, "series_index must be 0 to 19.");
  const every = b.check_every ?? "hour";
  if (every !== "hour" && every !== "day") throw new ApiError(400, "Check every hour or every day.");
  const direction = b.direction ?? (kind === "anomaly" ? "either" : undefined);
  if (kind === "threshold") {
    if (direction !== "above" && direction !== "below") throw new ApiError(400, "A threshold alert fires above or below the number.");
    if (typeof b.threshold !== "number" || !Number.isFinite(b.threshold)) throw new ApiError(400, "Give the threshold as a number.");
  } else if (direction !== "above" && direction !== "below" && direction !== "either") throw new ApiError(400, "An anomaly alert fires above, below or either way.");
  const sensitivity = kind === "anomaly" ? (b.sensitivity ?? 3) : null;
  if (sensitivity !== null && (typeof sensitivity !== "number" || !(sensitivity >= 1 && sensitivity <= 10))) throw new ApiError(400, "Sensitivity is 1 to 10 standard deviations.");
  return {
    insight_id: b.insight_id,
    name,
    kind,
    series_index: series,
    direction: direction as NewAlert["direction"],
    threshold: kind === "threshold" ? (b.threshold as number) : null,
    sensitivity: sensitivity as number | null,
    check_every: every,
    recipients: checkRecipients(b.recipients),
  };
}

export async function createAlert(db: Db, projectId: string, a: NewAlert): Promise<Alert> {
  const insight = await getSavedInsight(db, projectId, a.insight_id);
  if (!insight) throw new ApiError(404, "That insight does not exist.");
  if (a.kind === "anomaly" && insight.kind !== "trends" && insight.kind !== "lifecycle") throw new ApiError(400, "Anomaly alerts work on trends and lifecycle insights; use a threshold for this one.");
  return db.asProject(projectId, async (c) => {
    const n = (await c.query<{ n: number }>("select count(*)::int as n from pa.alerts where project_id = $1", [projectId])).rows[0].n;
    if (n >= MAX_ALERTS) throw new ApiError(409, `An app can have ${MAX_ALERTS} alerts.`);
    try {
      const r = await c.query(
        `insert into pa.alerts (project_id, insight_id, name, kind, series_index, direction, threshold, sensitivity, check_every, recipients)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text[]) returning ${COLS}`,
        [projectId, a.insight_id, a.name, a.kind, a.series_index, a.direction, a.threshold, a.sensitivity, a.check_every, a.recipients],
      );
      return toAlert(r.rows[0]);
    } catch (err) {
      if ((err as { code?: string }).code === "23505") throw new ApiError(409, "An alert with that name exists.");
      throw err;
    }
  });
}

export function listAlerts(db: Db, projectId: string): Promise<Alert[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.alerts where project_id = $1 order by created_at`, [projectId])).rows.map(toAlert));
}

export async function updateAlert(db: Db, projectId: string, id: string, b: Record<string, unknown>): Promise<Alert> {
  if (b.enabled !== undefined && typeof b.enabled !== "boolean") throw new ApiError(400, "enabled must be true or false.");
  const recipients = b.recipients === undefined ? null : checkRecipients(b.recipients);
  return db.asProject(projectId, async (c) => {
    const r = await c.query(
      `update pa.alerts set enabled = coalesce($3, enabled), recipients = coalesce($4::text[], recipients),
         next_check_at = case when $5 then null else next_check_at end, updated_at = now()
       where project_id = $1 and id = $2 returning ${COLS}`,
      [projectId, id, b.enabled ?? null, recipients, b.check_now === true],
    );
    if (!r.rows[0]) throw new ApiError(404, "That alert does not exist.");
    return toAlert(r.rows[0]);
  });
}

export async function deleteAlert(db: Db, projectId: string, id: string): Promise<void> {
  const n = await db.asProject(projectId, async (c) => (await c.query("delete from pa.alerts where project_id = $1 and id = $2", [projectId, id])).rowCount);
  if (!n) throw new ApiError(404, "That alert does not exist.");
}

/** Whether the value fires the alert, and the words for it. */
export function evaluate(a: Pick<Alert, "kind" | "direction" | "threshold" | "sensitivity">, value: number, history: number[]): { firing: boolean; why: string } {
  if (a.kind === "threshold") {
    const t = a.threshold as number;
    const firing = a.direction === "above" ? value > t : value < t;
    return { firing, why: `${value} is ${firing ? "" : "not "}${a.direction} ${t}` };
  }
  const z = zScore(value, history);
  if (z === null) return { firing: false, why: "fewer than 7 earlier intervals to compare with" };
  const s = a.sensitivity as number;
  const firing = a.direction === "above" ? z >= s : a.direction === "below" ? z <= -s : Math.abs(z) >= s;
  const zText = Number.isFinite(z) ? z.toFixed(2) : z > 0 ? "far above" : "far below";
  return { firing, why: `${value} is ${zText} standard deviations from the earlier ${history.length} intervals` };
}

function alertMail(a: Alert, insightName: string, value: number, why: string, consoleUrl: string) {
  const link = `${consoleUrl.replace(/\/+$/, "")}/product-analytics/insights/${a.insight_id}`;
  const subject = `Alert: ${a.name}`;
  const html = `<div style="font-family:system-ui,sans-serif;max-width:560px">
<h2 style="font-size:18px">${escapeHtml(a.name)}</h2>
<p>The insight <strong>${escapeHtml(insightName)}</strong> is at <strong>${escapeHtml(String(value))}</strong>: ${escapeHtml(why)}.</p>
<p><a href="${escapeHtml(link)}">Open the insight</a></p>
</div>`;
  const textBody = `${a.name}\n\nThe insight "${insightName}" is at ${value}: ${why}.\n\nOpen the insight: ${link}`;
  return { subject, html, text: textBody };
}

/** Checks one alert now; mails when it starts firing. */
export async function checkAlert(db: Db, raw: Alert, now: Date, mail: SendDeps & { consoleUrl: string }): Promise<Alert["state"]> {
  const a = raw;
  const next = new Date(now.getTime() + EVERY_MS[a.check_every]).toISOString();
  const save = (state: Alert["state"], value: number | null, error: string | null, fired: boolean) =>
    db.asProject(a.project_id, (c) =>
      c.query(
        `update pa.alerts set state = $3, last_value = $4, last_error = $5, last_checked_at = $6, next_check_at = $7,
           last_fired_at = case when $8 then $6::timestamptz else last_fired_at end, updated_at = $6
         where project_id = $1 and id = $2`,
        [a.project_id, a.id, state, value, error, now.toISOString(), next, fired],
      ),
    );
  const insight = await getSavedInsight(db, a.project_id, a.insight_id);
  if (!insight) {
    await save("error", null, "The insight was removed.", false);
    return "error";
  }
  let value: number | null;
  let history: number[];
  try {
    const answer = await computeCached(db, a.project_id, parseQuery(insight.query), now, "force");
    ({ value, history } = headline(answer.result, a.series_index));
  } catch (err) {
    await save("error", null, `The insight could not be computed: ${(err as Error).message}`.slice(0, 300), false);
    return "error";
  }
  if (value === null) {
    await save("ok", null, "The insight has no value yet.", false);
    return "ok";
  }
  const { firing, why } = evaluate(a, value, history);
  const starts = firing && a.state !== "firing";
  if (starts) {
    const m = alertMail(a, insight.name, value, why, mail.consoleUrl);
    await sendAll(mail, { projectId: a.project_id, source: "alert", sourceId: a.id, recipients: a.recipients, ...m, idempotencyKey: `alert:${a.id}:${now.toISOString()}` });
  }
  await save(firing ? "firing" : "ok", value, null, starts);
  return firing ? "firing" : "ok";
}

/** One scheduler pass: every due alert of every project, leased first. */
export async function runAlerts(db: Db, now: Date, mail: SendDeps & { consoleUrl: string }): Promise<Record<string, string>> {
  const due = await db.asOwner(async (c) =>
    (
      await c.query(
        `update pa.alerts set next_check_at = $2
         where id in (select id from pa.alerts where enabled and (next_check_at is null or next_check_at <= $1) order by next_check_at nulls first limit 100 for update skip locked)
         returning ${COLS}`,
        [now.toISOString(), new Date(now.getTime() + LEASE_MS).toISOString()],
      )
    ).rows.map(toAlert),
  );
  const out: Record<string, string> = {};
  for (const a of due) {
    try {
      out[a.id] = await checkAlert(db, a, now, mail);
    } catch (err) {
      out[a.id] = `error: ${(err as Error).message}`;
    }
  }
  return out;
}
