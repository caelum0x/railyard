// Dashboard subscriptions (QY1.4, A25): a dashboard mailed weekly, on a
// weekday and hour (UTC), as an HTML table: one row per tile with its
// watched value (metric.ts) and the value of the interval before. No
// images are needed. Sends are leased like alerts; a missed hour is sent at
// the next pass and the following send stays on the weekly rhythm.

import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { getDashboard } from "../insights/store.ts";
import { ApiError } from "../replay/shared/console-guard.ts";
import { checkRecipients } from "./alerts.ts";
import { escapeHtml } from "./mail.ts";
import { headline } from "./metric.ts";
import { sendAll, type SendDeps } from "./send.ts";

export const MAX_SUBSCRIPTIONS = 20;
const WEEK_MS = 7 * 86_400_000;
const LEASE_MS = 10 * 60_000;
export const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

export interface Subscription {
  id: string;
  project_id: string;
  dashboard_id: string;
  frequency: "weekly";
  weekday: number;
  hour_utc: number;
  recipients: string[];
  enabled: boolean;
  next_send_at: string;
  last_sent_at: string | null;
  last_error: string | null;
  created_at: string;
}

const COLS = "id, project_id, dashboard_id, frequency, weekday, hour_utc, recipients, enabled, next_send_at, last_sent_at, last_error, created_at";
const iso = (v: unknown) => (v instanceof Date ? v.toISOString() : ((v as string | null) ?? null));
const toSub = (r: Record<string, unknown>): Subscription => ({ ...(r as unknown as Subscription), next_send_at: iso(r.next_send_at)!, last_sent_at: iso(r.last_sent_at), created_at: iso(r.created_at)! });

/** The first weekday/hour (UTC) strictly after `now`. */
export function nextSlot(now: Date, weekday: number, hour: number): Date {
  const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), hour));
  d.setUTCDate(d.getUTCDate() + ((weekday - d.getUTCDay() + 7) % 7));
  if (d <= now) d.setUTCDate(d.getUTCDate() + 7);
  return d;
}

export async function createSubscription(db: Db, projectId: string, b: Record<string, unknown>, now: Date): Promise<Subscription> {
  if (typeof b.dashboard_id !== "string" || !isUuid(b.dashboard_id)) throw new ApiError(400, "Pick a dashboard.");
  const weekday = b.weekday ?? 1;
  const hour = b.hour_utc ?? 8;
  if (typeof weekday !== "number" || !Number.isInteger(weekday) || weekday < 0 || weekday > 6) throw new ApiError(400, "weekday is 0 (Sunday) to 6 (Saturday).");
  if (typeof hour !== "number" || !Number.isInteger(hour) || hour < 0 || hour > 23) throw new ApiError(400, "hour_utc is 0 to 23.");
  const recipients = checkRecipients(b.recipients);
  return db.asProject(projectId, async (c) => {
    const d = (await c.query("select 1 from pa.dashboards where project_id = $1 and id = $2", [projectId, b.dashboard_id])).rows[0];
    if (!d) throw new ApiError(404, "That dashboard does not exist.");
    const n = (await c.query<{ n: number }>("select count(*)::int as n from pa.subscriptions where project_id = $1", [projectId])).rows[0].n;
    if (n >= MAX_SUBSCRIPTIONS) throw new ApiError(409, `An app can have ${MAX_SUBSCRIPTIONS} subscriptions.`);
    const r = await c.query(
      `insert into pa.subscriptions (project_id, dashboard_id, weekday, hour_utc, recipients, next_send_at) values ($1, $2, $3, $4, $5::text[], $6) returning ${COLS}`,
      [projectId, b.dashboard_id, weekday, hour, recipients, nextSlot(now, weekday, hour).toISOString()],
    );
    return toSub(r.rows[0]);
  });
}

export function listSubscriptions(db: Db, projectId: string): Promise<Subscription[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.subscriptions where project_id = $1 order by created_at`, [projectId])).rows.map(toSub));
}

export async function updateSubscription(db: Db, projectId: string, id: string, b: Record<string, unknown>, now: Date): Promise<Subscription> {
  if (b.enabled !== undefined && typeof b.enabled !== "boolean") throw new ApiError(400, "enabled must be true or false.");
  const recipients = b.recipients === undefined ? null : checkRecipients(b.recipients);
  return db.asProject(projectId, async (c) => {
    const r = await c.query(
      `update pa.subscriptions set enabled = coalesce($3, enabled), recipients = coalesce($4::text[], recipients),
         next_send_at = case when $5 then $6::timestamptz else next_send_at end, updated_at = $6
       where project_id = $1 and id = $2 returning ${COLS}`,
      [projectId, id, b.enabled ?? null, recipients, b.send_now === true, now.toISOString()],
    );
    if (!r.rows[0]) throw new ApiError(404, "That subscription does not exist.");
    return toSub(r.rows[0]);
  });
}

export async function deleteSubscription(db: Db, projectId: string, id: string): Promise<void> {
  const n = await db.asProject(projectId, async (c) => (await c.query("delete from pa.subscriptions where project_id = $1 and id = $2", [projectId, id])).rowCount);
  if (!n) throw new ApiError(404, "That subscription does not exist.");
}

const fmt = (n: number | null) => (n === null ? "-" : Number.isInteger(n) ? n.toLocaleString("en-US") : n.toLocaleString("en-US", { maximumFractionDigits: 2 }));

/** The dashboard as an HTML table and plain text (no images). */
export function renderDashboard(name: string, rows: { insight: string; label: string; value: number | null; previous: number | null; error: string | null }[], link: string): { subject: string; html: string; text: string } {
  const cell = "padding:6px 10px;border-bottom:1px solid #eee;text-align:left";
  const body = rows
    .map((r) => `<tr><td style="${cell}">${escapeHtml(r.insight)}<br><span style="color:#666;font-size:12px">${escapeHtml(r.error ?? r.label)}</span></td><td style="${cell}">${fmt(r.value)}</td><td style="${cell}">${fmt(r.previous)}</td></tr>`)
    .join("\n");
  const html = `<div style="font-family:system-ui,sans-serif;max-width:640px">
<h2 style="font-size:18px">${escapeHtml(name)}</h2>
<table style="border-collapse:collapse;width:100%;font-size:14px">
<thead><tr><th style="${cell}">Insight</th><th style="${cell}">Latest</th><th style="${cell}">Interval before</th></tr></thead>
<tbody>
${body || `<tr><td style="${cell}" colspan="3">This dashboard has no insights yet.</td></tr>`}
</tbody>
</table>
<p><a href="${escapeHtml(link)}">Open the dashboard</a></p>
</div>`;
  const text = `${name}\n\n${rows.map((r) => `${r.insight}: ${fmt(r.value)} (interval before: ${fmt(r.previous)})${r.error ? ` - ${r.error}` : ""}`).join("\n") || "This dashboard has no insights yet."}\n\nOpen the dashboard: ${link}`;
  return { subject: `Weekly: ${name}`, html, text };
}

/** Sends one subscription now. */
export async function sendSubscription(db: Db, s: Subscription, now: Date, mail: SendDeps & { consoleUrl: string }): Promise<{ sent: number; failed: number }> {
  const d = await getDashboard(db, s.project_id, s.dashboard_id, now, { refresh: false });
  if (!d) throw new Error("The dashboard was removed.");
  const rows = d.tiles.map((t) => {
    if (!t.answer) return { insight: t.insight.name, label: "", value: null, previous: null, error: t.error ?? "Could not be computed." };
    const h = headline(t.answer.result, 0);
    return { insight: t.insight.name, label: h.label, value: h.value, previous: h.history.length ? h.history[h.history.length - 1] : null, error: null };
  });
  const m = renderDashboard(d.name, rows, `${mail.consoleUrl.replace(/\/+$/, "")}/product-analytics/dashboards/${d.id}`);
  return sendAll(mail, { projectId: s.project_id, source: "subscription", sourceId: s.id, recipients: s.recipients, ...m, idempotencyKey: `subscription:${s.id}:${s.next_send_at}` });
}

/** One scheduler pass over due subscriptions. */
export async function runSubscriptions(db: Db, now: Date, mail: SendDeps & { consoleUrl: string }): Promise<Record<string, string>> {
  // Lease: the slot moves LEASE_MS ahead while this pass sends; the outcome sets the next weekly slot.
  const due = await db.asOwner(async (c) =>
    (
      await c.query(
        `with due as (
           select id, next_send_at as slot from pa.subscriptions
            where enabled and next_send_at <= $1 order by next_send_at limit 50 for update skip locked)
         update pa.subscriptions s set next_send_at = $2 from due where s.id = due.id
         returning ${COLS.split(", ").map((c) => (c === "next_send_at" ? "due.slot as next_send_at" : `s.${c}`)).join(", ")}`,
        [now.toISOString(), new Date(now.getTime() + LEASE_MS).toISOString()],
      )
    ).rows.map(toSub),
  );
  const out: Record<string, string> = {};
  for (const s of due) {
    let next = new Date(s.next_send_at).getTime() + WEEK_MS;
    while (next <= now.getTime()) next += WEEK_MS;
    try {
      const r = await sendSubscription(db, s, now, mail);
      out[s.id] = `sent ${r.sent}, failed ${r.failed}`;
      await db.asProject(s.project_id, (c) =>
        c.query("update pa.subscriptions set next_send_at = $3, last_sent_at = $4, last_error = null where project_id = $1 and id = $2", [s.project_id, s.id, new Date(next).toISOString(), now.toISOString()]),
      );
    } catch (err) {
      out[s.id] = `error: ${(err as Error).message}`;
      await db.asProject(s.project_id, (c) =>
        c.query("update pa.subscriptions set next_send_at = $3, last_error = $4 where project_id = $1 and id = $2", [s.project_id, s.id, new Date(next).toISOString(), (err as Error).message.slice(0, 300)]),
      );
    }
  }
  return out;
}
