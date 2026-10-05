// Export delivery. Every due destination gets the project's new events in
// order of (inserted_at, uuid) after its cursor, in batches, read inside a
// transaction scoped to that destination's project (row level security plus
// a bound project_id: a destination never receives another project's rows).
// Only rows inserted at least CURSOR_LAG_MS ago are read, so a batch insert
// that commits late is never skipped. A failed batch keeps the cursor and is
// retried with exponential backoff (30 s doubling, at most an hour).
//   webhook: POST {type, project_id, batch_id, events[]} signed with Standard Webhooks
//   s3:      one NDJSON.gz object per batch under <prefix>/events/<project>/<yyyy>/<mm>/<dd>/
//   slack, discord, hubspot (QY1.4): catalogue.ts turns the batch into the target's request

import { randomUUID } from "node:crypto";
import { gzipSync } from "node:zlib";
import { sign } from "../bridge/standard-webhooks.ts";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { S3ObjectStore } from "../replay/object-store.ts";
import { toDestination, type Destination, type S3DestConfig, type WebhookConfig } from "./destinations.ts";
import { checkReachable, checkUrlShape } from "./net-guard.ts";
import { open } from "./secrets.ts";
import { isCatalogue, openBundle, sendCatalogue, type SecretBundle } from "./catalogue.ts";

export const BATCH_SIZE = 500;
export const MAX_BATCHES_PER_RUN = 20;
export const CURSOR_LAG_MS = 60_000;
export const LEASE_MS = 5 * 60_000;
export const BASE_BACKOFF_MS = 30_000;
export const MAX_BACKOFF_MS = 3600_000;
export const FAILING_AFTER = 8;
export const SEND_TIMEOUT_MS = 10_000;

export const backoffMs = (attempts: number): number => Math.min(MAX_BACKOFF_MS, BASE_BACKOFF_MS * 2 ** Math.max(0, attempts - 1));

export interface ExportedEvent {
  uuid: string;
  event: string;
  distinct_id: string;
  person_id: string;
  timestamp: string;
  properties: Record<string, unknown>;
  elements_chain: string | null;
  session_id: string | null;
}

interface Row extends ExportedEvent {
  inserted_at: Date;
  /** inserted_at as Postgres text: microseconds kept, so the cursor is exact. */
  inserted_text: string;
}

export interface RunnerOptions {
  fetchImpl?: typeof fetch;
  log: Logger;
  batchSize?: number;
}

export class DeliveryError extends Error {
  httpStatus: number | null;
  constructor(message: string, httpStatus: number | null = null) {
    super(message);
    this.httpStatus = httpStatus;
  }
}

type Cursor = { cursor_uuid: string | null; cursor_text: string | null };

async function nextBatch(db: Db, d: Destination & Cursor, now: Date, size: number): Promise<Row[]> {
  return db.asProject(d.project_id, async (c) => {
    const r = await c.query<Row>(
      `select uuid, event, distinct_id, person_id, "timestamp", properties, elements_chain, session_id, inserted_at, inserted_at::text as inserted_text
       from pa.events
       where project_id = $1 and inserted_at < $2
         and ($3::timestamptz is null or (inserted_at, uuid) > ($3::timestamptz, coalesce($4::uuid, '00000000-0000-0000-0000-000000000000'::uuid)))
         and ($5::text[] is null or event = any($5::text[]))
       order by inserted_at, uuid
       limit $6`,
      [d.project_id, new Date(now.getTime() - CURSOR_LAG_MS).toISOString(), d.cursor_text, d.cursor_uuid, d.events, size],
    );
    return r.rows;
  });
}

const exported = (r: Row): ExportedEvent => ({
  uuid: r.uuid,
  event: r.event,
  distinct_id: r.distinct_id,
  person_id: r.person_id,
  timestamp: new Date(r.timestamp).toISOString(),
  properties: r.properties,
  elements_chain: r.elements_chain,
  session_id: r.session_id,
});

export async function sendWebhook(cfg: WebhookConfig, secret: string, body: string, messageId: string, now: Date, fetchImpl: typeof fetch): Promise<number> {
  const url = checkUrlShape(cfg.url, "The webhook URL");
  await checkReachable(url);
  const ts = String(Math.floor(now.getTime() / 1000));
  let res: Response;
  try {
    res = await fetchImpl(url, {
      method: "POST",
      redirect: "manual",
      signal: AbortSignal.timeout(SEND_TIMEOUT_MS),
      headers: { "content-type": "application/json", "user-agent": "railyard-product-analytics-export/1", "webhook-id": messageId, "webhook-timestamp": ts, "webhook-signature": sign(secret, messageId, ts, body) },
      body,
    });
  } catch (err) {
    throw new DeliveryError(`The webhook could not be reached: ${(err as Error).message}`.slice(0, 300));
  }
  await res.arrayBuffer().catch(() => undefined);
  if (res.status < 200 || res.status >= 300) throw new DeliveryError(`The webhook answered ${res.status}.`, res.status);
  return res.status;
}

async function sendS3(d: Destination, secret: string, rows: Row[], batchId: string, fetchImpl: typeof fetch): Promise<number> {
  const cfg = d.config as S3DestConfig;
  const url = checkUrlShape(cfg.endpoint, "The endpoint");
  await checkReachable(url);
  const store = new S3ObjectStore({ endpoint: cfg.endpoint, bucket: cfg.bucket, region: cfg.region, accessKeyId: cfg.access_key_id, secretAccessKey: secret, prefix: cfg.prefix }, fetchImpl);
  const last = rows[rows.length - 1].inserted_at;
  const day = last.toISOString().slice(0, 10).split("-");
  const key = `events/${d.project_id}/${day[0]}/${day[1]}/${day[2]}/${last.getTime()}-${batchId}.ndjson.gz`;
  const body = gzipSync(Buffer.from(rows.map((r) => JSON.stringify(exported(r))).join("\n") + "\n", "utf8"));
  try {
    await store.put(key, body, "application/x-ndjson");
  } catch (err) {
    throw new DeliveryError(`The bucket did not take the file: ${(err as Error).message}`.slice(0, 300), (err as { status?: number }).status ?? null);
  }
  return body.length;
}

async function record(db: Db, d: Destination, started: Date, finished: Date, attempt: number, rows: number, bytes: number, status: "ok" | "failed", httpStatus: number | null, error: string | null) {
  await db.asOwner((c) =>
    c.query(
      `insert into pa.export_runs (project_id, destination_id, started_at, finished_at, attempt, rows, bytes, status, http_status, error)
       values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`,
      [d.project_id, d.id, started.toISOString(), finished.toISOString(), attempt, rows, bytes, status, httpStatus, error],
    ),
  );
}

/** Delivers what one destination has waiting (up to MAX_BATCHES_PER_RUN batches). Returns rows delivered. */
export async function deliver(db: Db, raw: Record<string, unknown>, now: Date, opts: RunnerOptions): Promise<number> {
  const d: Destination & Cursor = { ...toDestination(raw), cursor_uuid: (raw.cursor_uuid as string | null) ?? null, cursor_text: (raw.cursor_text as string | null) ?? null };
  const fetchImpl = opts.fetchImpl ?? fetch;
  let secret: string;
  let bundle: SecretBundle | null = null;
  try {
    secret = open(String(raw.secret_enc));
    if (isCatalogue(d.kind)) bundle = openBundle(secret);
  } catch (err) {
    opts.log.error("export secret could not be opened", { destination: d.id, error: (err as Error).message });
    await failed(db, d, now, `The destination's secret could not be read on this deployment.`, null, 0, 0);
    return 0;
  }
  let delivered = 0;
  for (let i = 0; i < MAX_BATCHES_PER_RUN; i += 1) {
    const rows = await nextBatch(db, d, now, opts.batchSize ?? BATCH_SIZE);
    if (rows.length === 0) break;
    const batchId = randomUUID();
    const t0 = Date.now();
    const started = now;
    let bytes = 0;
    let httpStatus: number | null = null;
    try {
      if (d.kind === "webhook") {
        const body = JSON.stringify({ type: "events", project_id: d.project_id, batch_id: batchId, events: rows.map(exported) });
        bytes = Buffer.byteLength(body);
        httpStatus = await sendWebhook(d.config as WebhookConfig, secret, body, `msg_${batchId}`, now, fetchImpl);
      } else if (isCatalogue(d.kind) && bundle) {
        const sent = await sendCatalogue(d.kind, bundle, rows.map(exported), `msg_${batchId}`, now, fetchImpl);
        bytes = sent.bytes;
        httpStatus = sent.status;
      } else {
        bytes = await sendS3(d, secret, rows, batchId, fetchImpl);
      }
    } catch (err) {
      const e = err instanceof DeliveryError ? err : new DeliveryError((err as Error).message.slice(0, 300));
      await failed(db, d, now, e.message, e.httpStatus, rows.length, bytes, Date.now() - t0);
      return delivered;
    }
    const last = rows[rows.length - 1];
    await db.asOwner((c) =>
      c.query(
        `update pa.export_destinations set cursor_inserted_at = $3, cursor_uuid = $4, attempts = 0, status = 'ok', last_error = null,
           last_success_at = $5, next_attempt_at = null, delivered = delivered + $6, updated_at = $5
         where project_id = $1 and id = $2`,
        [d.project_id, d.id, last.inserted_text, last.uuid, now.toISOString(), rows.length],
      ),
    );
    await record(db, d, started, new Date(now.getTime() + (Date.now() - t0)), d.attempts + 1, rows.length, bytes, "ok", httpStatus, null);
    d.cursor_text = last.inserted_text;
    d.cursor_uuid = last.uuid;
    d.attempts = 0;
    delivered += rows.length;
    if (rows.length < (opts.batchSize ?? BATCH_SIZE)) break;
  }
  if (delivered === 0) await db.asOwner((c) => c.query("update pa.export_destinations set next_attempt_at = null where project_id = $1 and id = $2 and attempts = 0", [d.project_id, d.id]));
  return delivered;
}

async function failed(db: Db, d: Destination, now: Date, message: string, httpStatus: number | null, rows: number, bytes: number, tookMs = 0) {
  const attempts = d.attempts + 1;
  await db.asOwner((c) =>
    c.query(
      `update pa.export_destinations set attempts = $3, status = $4, last_error = $5, next_attempt_at = $6, updated_at = $7 where project_id = $1 and id = $2`,
      [d.project_id, d.id, attempts, attempts >= FAILING_AFTER ? "failing" : "retrying", message, new Date(now.getTime() + backoffMs(attempts)).toISOString(), now.toISOString()],
    ),
  );
  await record(db, d, now, new Date(now.getTime() + tookMs), attempts, rows, bytes, "failed", httpStatus, message);
}

/** One pass over every due destination: leased first, so two passes never send the same batch. */
export async function runExports(db: Db, now: Date, opts: RunnerOptions): Promise<Record<string, number>> {
  const due = await db.asOwner(async (c) => {
    const r = await c.query(
      `update pa.export_destinations set next_attempt_at = $2
       where id in (select id from pa.export_destinations where enabled and (next_attempt_at is null or next_attempt_at <= $1) order by next_attempt_at nulls first limit 50 for update skip locked)
       returning *, cursor_inserted_at::text as cursor_text`,
      [now.toISOString(), new Date(now.getTime() + LEASE_MS).toISOString()],
    );
    return r.rows;
  });
  const out: Record<string, number> = {};
  for (const raw of due) {
    // The lease is replaced by the outcome: a success clears it, a failure sets the backoff.
    const leased = { ...raw, next_attempt_at: null };
    try {
      out[String(raw.id)] = await deliver(db, leased, now, opts);
    } catch (err) {
      opts.log.error("export run failed", { destination: String(raw.id), error: (err as Error).message });
    }
  }
  return out;
}
