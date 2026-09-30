// Export destinations per project (pa.export_destinations): a signed webhook
// or an S3-compatible bucket. The secret (the webhook signing secret the
// service makes, or the bucket's secret key the customer gives) is sealed at
// rest and never read back through the API; a webhook's secret is shown once,
// when it is created. At most MAX_DESTINATIONS per project (target).

import type { Client, Db } from "../db.ts";
import { BUCKET } from "../replay/object-store.ts";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { checkUrlShape, DestinationRefused } from "./net-guard.ts";
import { newSigningSecret, seal, SecretsUnavailable } from "./secrets.ts";

/** Destinations per project (target, copy-source: apps/product-analytics/src/exports/destinations.ts). */
export const MAX_DESTINATIONS = 3;
export const MAX_EVENT_FILTER = 50;

export type WebhookConfig = { url: string };
export type S3DestConfig = { endpoint: string; bucket: string; region: string; prefix: string; access_key_id: string };

export interface Destination {
  id: string;
  project_id: string;
  name: string;
  kind: "webhook" | "s3";
  config: WebhookConfig | S3DestConfig;
  events: string[] | null;
  enabled: boolean;
  status: "ok" | "retrying" | "failing";
  attempts: number;
  next_attempt_at: string | null;
  last_error: string | null;
  last_success_at: string | null;
  delivered: number;
  cursor_inserted_at: string | null;
  created_at: string;
}

const COLS = "id, project_id, name, kind, config, events, enabled, status, attempts, next_attempt_at, last_error, last_success_at, delivered::float8 as delivered, cursor_inserted_at, created_at";
const iso = (v: unknown) => (v instanceof Date ? v.toISOString() : ((v as string | null) ?? null));
export const toDestination = (r: Record<string, unknown>): Destination => ({
  ...(r as unknown as Destination),
  delivered: Number(r.delivered),
  next_attempt_at: iso(r.next_attempt_at),
  last_success_at: iso(r.last_success_at),
  cursor_inserted_at: iso(r.cursor_inserted_at),
  created_at: iso(r.created_at)!,
});

export interface NewDestination {
  name: string;
  kind: "webhook" | "s3";
  config: WebhookConfig | S3DestConfig;
  secret: string;
  events: string[] | null;
  start: "now" | "all";
}

function guard<T>(fn: () => T): T {
  try {
    return fn();
  } catch (err) {
    if (err instanceof DestinationRefused) throw new ApiError(400, err.message);
    throw err;
  }
}

export function checkDestination(b: Record<string, unknown>): NewDestination {
  const name = text(b.name, "Name", 200)!;
  const events = b.events === undefined || b.events === null ? null : b.events;
  if (events !== null && (!Array.isArray(events) || events.length === 0 || events.length > MAX_EVENT_FILTER || !events.every((e) => typeof e === "string" && e.length >= 1 && e.length <= 200))) {
    throw new ApiError(400, `Name 1 to ${MAX_EVENT_FILTER} events, or leave the list out to send every event.`);
  }
  const start = b.start ?? "now";
  if (start !== "now" && start !== "all") throw new ApiError(400, "Start from now or from all kept events.");
  const c = (b.config ?? {}) as Record<string, unknown>;
  if (b.kind === "webhook") {
    const url = guard(() => checkUrlShape(text(c.url, "The webhook URL", 2000)!, "The webhook URL")).toString();
    return { name, kind: "webhook", config: { url }, secret: newSigningSecret(), events: events as string[] | null, start };
  }
  if (b.kind === "s3") {
    const endpoint = guard(() => checkUrlShape(text(c.endpoint, "The endpoint", 500)!, "The endpoint")).origin;
    const bucket = text(c.bucket, "The bucket", 63)!;
    if (!BUCKET.test(bucket)) throw new ApiError(400, "The bucket name is not valid.");
    const prefix = (text(c.prefix, "The prefix", 200, false) ?? "").replace(/^\/+|\/+$/g, "");
    if (prefix && !/^[A-Za-z0-9/_.=-]+$/.test(prefix)) throw new ApiError(400, "The prefix may use letters, digits and / _ . = -.");
    const region = text(c.region, "The region", 50, false) ?? "auto";
    if (!/^[a-z0-9-]{1,50}$/.test(region)) throw new ApiError(400, "The region is not valid.");
    const access = text(c.access_key_id, "The access key id", 200)!;
    const secret = text(b.secret, "The secret access key", 500)!;
    return { name, kind: "s3", config: { endpoint, bucket, region, prefix, access_key_id: access }, secret, events: events as string[] | null, start };
  }
  throw new ApiError(400, "The destination kind must be webhook or s3.");
}

export async function createDestination(db: Db, projectId: string, d: NewDestination, now: Date): Promise<Destination> {
  let sealed: string;
  try {
    sealed = seal(d.secret);
  } catch (err) {
    if (err instanceof SecretsUnavailable) throw new ApiError(503, "Exports are not set up on this deployment yet.");
    throw err;
  }
  return db.asProject(projectId, async (c) => {
    const n = (await c.query<{ n: number }>("select count(*)::int as n from pa.export_destinations where project_id = $1", [projectId])).rows[0].n;
    if (n >= MAX_DESTINATIONS) throw new ApiError(409, `An app can have ${MAX_DESTINATIONS} export destinations. Remove one first.`);
    try {
      const r = await c.query(
        `insert into pa.export_destinations (project_id, name, kind, config, secret_enc, events, cursor_inserted_at, created_at, updated_at)
         values ($1, $2, $3, $4::jsonb, $5, $6::text[], $7, $8, $8) returning ${COLS}`,
        [projectId, d.name, d.kind, JSON.stringify(d.config), sealed, d.events, d.start === "now" ? now.toISOString() : null, now.toISOString()],
      );
      return toDestination(r.rows[0]);
    } catch (err) {
      if ((err as { code?: string }).code === "23505") throw new ApiError(409, "A destination with that name exists.");
      throw err;
    }
  });
}

export function listDestinations(db: Db, projectId: string): Promise<Destination[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.export_destinations where project_id = $1 order by created_at`, [projectId])).rows.map(toDestination));
}

export async function getDestination(c: Client, projectId: string, id: string): Promise<Destination> {
  const r = await c.query(`select ${COLS} from pa.export_destinations where project_id = $1 and id = $2`, [projectId, id]);
  if (!r.rows[0]) throw new ApiError(404, "That destination does not exist.");
  return toDestination(r.rows[0]);
}

/** Pause or resume; retry now clears the wait. */
export async function updateDestination(db: Db, projectId: string, id: string, b: Record<string, unknown>, now: Date): Promise<Destination> {
  return db.asProject(projectId, async (c) => {
    await getDestination(c, projectId, id);
    if (b.enabled !== undefined && typeof b.enabled !== "boolean") throw new ApiError(400, "enabled must be true or false.");
    const r = await c.query(
      `update pa.export_destinations set enabled = coalesce($3, enabled),
         next_attempt_at = case when $4 then null else next_attempt_at end, updated_at = $5
       where project_id = $1 and id = $2 returning ${COLS}`,
      [projectId, id, b.enabled ?? null, b.retry_now === true, now.toISOString()],
    );
    return toDestination(r.rows[0]);
  });
}

export async function deleteDestination(db: Db, projectId: string, id: string): Promise<void> {
  await db.asProject(projectId, async (c) => {
    await getDestination(c, projectId, id);
    await c.query("delete from pa.export_destinations where project_id = $1 and id = $2", [projectId, id]);
  });
}

export async function listRuns(db: Db, projectId: string, id: string, limit = 50) {
  return db.asProject(projectId, async (c) => {
    await getDestination(c, projectId, id);
    const r = await c.query(
      `select started_at, finished_at, attempt, rows, bytes, status, http_status, error from pa.export_runs where project_id = $1 and destination_id = $2 order by started_at desc limit $3`,
      [projectId, id, limit],
    );
    return r.rows.map((x) => ({ ...x, started_at: iso(x.started_at), finished_at: iso(x.finished_at) }));
  });
}
