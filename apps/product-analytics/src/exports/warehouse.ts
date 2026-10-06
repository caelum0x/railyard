// Warehouse destinations (A22): a project's events land as rows in the
// customer's own ClickHouse or BigQuery table, next to the S3 file export.
// Delivery, retries with backoff, the run log and the on/off switch are the
// exports runner's, like every catalogue destination.
//
// ClickHouse (HTTP interface): the table is created when missing as a
// ReplacingMergeTree ordered by the event uuid, so a retried batch collapses
// to one row per event; rows go in as JSONEachRow. Credentials are the
// customer's user name (kept in the clear) and password (sealed).
//
// BigQuery (REST, tabledata.insertAll): the customer's service account key
// (sealed) signs a 1-hour RS256 assertion exchanged for an access token; each
// row carries the event uuid as insertId (BigQuery's own retry dedupe), and a
// missing table is created with the event schema once.

import { createSign } from "node:crypto";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { allowPrivate, checkUrlShape, DestinationRefused } from "./net-guard.ts";
import { DeliveryError, SEND_TIMEOUT_MS, type ExportedEvent } from "./runner.ts";

export const WAREHOUSES = ["clickhouse", "bigquery"] as const;
export type WarehouseKind = (typeof WAREHOUSES)[number];
export const isWarehouse = (k: string): k is WarehouseKind => (WAREHOUSES as readonly string[]).includes(k);

const IDENT = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;
const BQ_PROJECT = /^[a-z][a-z0-9-]{4,28}[a-z0-9]$/;
export const BIGQUERY_URL = () => (process.env.PA_BIGQUERY_URL?.trim() || "https://bigquery.googleapis.com").replace(/\/$/, "");
const GOOGLE_TOKEN_URL = "https://oauth2.googleapis.com/token";
const BQ_SCOPE = "https://www.googleapis.com/auth/bigquery.insertdata https://www.googleapis.com/auth/bigquery";

/** The event row both warehouses store. */
export function warehouseRow(e: ExportedEvent): Record<string, string | null> {
  return {
    uuid: e.uuid,
    event: e.event,
    distinct_id: e.distinct_id,
    person_id: e.person_id,
    timestamp: e.timestamp,
    properties: JSON.stringify(e.properties ?? {}),
    elements_chain: e.elements_chain,
    session_id: e.session_id,
  };
}

function guardUrl(raw: string, what: string): URL {
  try {
    return checkUrlShape(raw, what);
  } catch (err) {
    if (err instanceof DestinationRefused) throw new ApiError(400, err.message);
    throw err;
  }
}

interface ServiceAccount {
  client_email: string;
  private_key: string;
  token_uri: string;
}

function parseServiceAccount(raw: string): ServiceAccount {
  let v: Partial<ServiceAccount> & { type?: string };
  try {
    v = JSON.parse(raw);
  } catch {
    throw new ApiError(400, "The BigQuery key must be the service account's JSON key file.");
  }
  if (typeof v.client_email !== "string" || !/^[^\s@]+@[^\s@]+$/.test(v.client_email)) throw new ApiError(400, "The BigQuery key has no client_email.");
  if (typeof v.private_key !== "string" || !v.private_key.includes("PRIVATE KEY")) throw new ApiError(400, "The BigQuery key has no private_key.");
  const tokenUri = typeof v.token_uri === "string" && v.token_uri ? v.token_uri : GOOGLE_TOKEN_URL;
  // Only Google's token endpoint receives the signed assertion (a local fake in tests).
  if (tokenUri !== GOOGLE_TOKEN_URL && !allowPrivate()) throw new ApiError(400, "The BigQuery key's token_uri must be Google's.");
  return { client_email: v.client_email, private_key: v.private_key, token_uri: tokenUri };
}

/** Validates a warehouse destination; returns the clear config and the target secret to seal. */
export function checkWarehouse(kind: WarehouseKind, c: Record<string, unknown>, secretIn: unknown): { config: Record<string, string>; target: string } {
  if (kind === "clickhouse") {
    const u = guardUrl(text(c.url, "The ClickHouse address", 500)!, "The ClickHouse address");
    const database = text(c.database, "The ClickHouse database", 128, false) ?? "default";
    const table = text(c.table, "The ClickHouse table", 128, false) ?? "railyard_events";
    const user = text(c.user, "The ClickHouse user", 128, false) ?? "default";
    for (const [what, v] of [["database", database], ["table", table]] as const) {
      if (!IDENT.test(v)) throw new ApiError(400, `The ClickHouse ${what} may use letters, digits and _ only.`);
    }
    const password = text(secretIn, "The ClickHouse password", 500, false) ?? "";
    return { config: { url: u.origin + u.pathname.replace(/\/$/, ""), database, table, user, label: text(c.label, "The label", 80, false) ?? "ClickHouse" }, target: JSON.stringify({ password }) };
  }
  const project = text(c.project_id, "The BigQuery project", 30)!;
  if (!BQ_PROJECT.test(project)) throw new ApiError(400, "The BigQuery project id is not valid.");
  const dataset = text(c.dataset, "The BigQuery dataset", 1024)!;
  const table = text(c.table, "The BigQuery table", 1024, false) ?? "railyard_events";
  for (const [what, v] of [["dataset", dataset], ["table", table]] as const) {
    if (!IDENT.test(v)) throw new ApiError(400, `The BigQuery ${what} may use letters, digits and _ only.`);
  }
  const key = text(secretIn, "The BigQuery service account key", 20_000)!;
  parseServiceAccount(key);
  return { config: { project_id: project, dataset, table, label: text(c.label, "The label", 80, false) ?? "BigQuery" }, target: key };
}

export function describeWarehouse(kind: WarehouseKind, config: Record<string, unknown>): string {
  return kind === "clickhouse"
    ? `ClickHouse: ${String(config.database ?? "")}.${String(config.table ?? "")}`
    : `BigQuery: ${String(config.project_id ?? "")}.${String(config.dataset ?? "")}.${String(config.table ?? "")}`;
}

async function call(fetchImpl: typeof fetch, url: string, init: RequestInit): Promise<{ status: number; body: string }> {
  let res: Response;
  try {
    res = await fetchImpl(url, { redirect: "manual", signal: AbortSignal.timeout(SEND_TIMEOUT_MS), ...init, headers: { "user-agent": "railyard-product-analytics-export/1", ...(init.headers as Record<string, string>) } });
  } catch (err) {
    if (err instanceof DestinationRefused) throw new DeliveryError(err.message.slice(0, 300));
    throw new DeliveryError(`The warehouse could not be reached: ${(err as Error).message}`.slice(0, 300));
  }
  return { status: res.status, body: await res.text().catch(() => "") };
}

function fail(what: string, r: { status: number; body: string }): never {
  // Never echo the warehouse's body in full (it may quote the query); a short excerpt helps.
  throw new DeliveryError(`${what} answered ${r.status}: ${r.body.replace(/\s+/g, " ").slice(0, 160)}`, r.status);
}

/** ClickHouse: create the table when missing, then insert the batch. */
async function sendClickHouse(config: Record<string, unknown>, password: string, events: ExportedEvent[], fetchImpl: typeof fetch): Promise<{ bytes: number; status: number }> {
  const base = String(config.url);
  const db = String(config.database);
  const table = String(config.table);
  if (!IDENT.test(db) || !IDENT.test(table)) throw new DeliveryError("The ClickHouse table name is not valid.");
  const headers = { "x-clickhouse-user": String(config.user ?? "default"), "x-clickhouse-key": password };
  const q = (sql: string) => `${base}/?query=${encodeURIComponent(sql)}`;
  const ddl =
    `CREATE TABLE IF NOT EXISTS \`${db}\`.\`${table}\` (uuid UUID, event String, distinct_id String, person_id Nullable(String), ` +
    `timestamp DateTime64(3, 'UTC'), properties String, elements_chain Nullable(String), session_id Nullable(String)) ` +
    `ENGINE = ReplacingMergeTree ORDER BY (event, uuid)`;
  const created = await call(fetchImpl, q(ddl), { method: "POST", headers });
  if (created.status < 200 || created.status >= 300) fail("ClickHouse", created);
  const body = events.map((e) => JSON.stringify({ ...warehouseRow(e), timestamp: e.timestamp.replace("T", " ").replace("Z", "") })).join("\n");
  const sent = await call(fetchImpl, q(`INSERT INTO \`${db}\`.\`${table}\` FORMAT JSONEachRow`), {
    method: "POST",
    headers: { ...headers, "content-type": "application/x-ndjson" },
    body,
  });
  if (sent.status < 200 || sent.status >= 300) fail("ClickHouse", sent);
  return { bytes: Buffer.byteLength(body), status: sent.status };
}

const b64url = (b: Buffer | string) => Buffer.from(b).toString("base64").replace(/=+$/, "").replace(/\+/g, "-").replace(/\//g, "_");

/** The signed JWT bearer assertion for a service account (RFC 7523). */
export function serviceAccountAssertion(sa: ServiceAccount, now: Date): string {
  const iat = Math.floor(now.getTime() / 1000);
  const head = b64url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const claims = b64url(JSON.stringify({ iss: sa.client_email, scope: BQ_SCOPE, aud: sa.token_uri, iat, exp: iat + 3600 }));
  const signer = createSign("RSA-SHA256");
  signer.update(`${head}.${claims}`);
  return `${head}.${claims}.${b64url(signer.sign(sa.private_key))}`;
}

const BQ_SCHEMA = {
  fields: [
    { name: "uuid", type: "STRING", mode: "REQUIRED" },
    { name: "event", type: "STRING", mode: "REQUIRED" },
    { name: "distinct_id", type: "STRING", mode: "REQUIRED" },
    { name: "person_id", type: "STRING" },
    { name: "timestamp", type: "TIMESTAMP", mode: "REQUIRED" },
    { name: "properties", type: "JSON" },
    { name: "elements_chain", type: "STRING" },
    { name: "session_id", type: "STRING" },
  ],
};

/** BigQuery: token, then insertAll; a missing table is created once. */
async function sendBigQuery(config: Record<string, unknown>, key: string, events: ExportedEvent[], now: Date, fetchImpl: typeof fetch): Promise<{ bytes: number; status: number }> {
  const sa = parseServiceAccount(key);
  const token = await call(fetchImpl, sa.token_uri, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer", assertion: serviceAccountAssertion(sa, now) }).toString(),
  });
  if (token.status !== 200) fail("Google sign-in", token);
  let access: string;
  try {
    access = String(JSON.parse(token.body).access_token ?? "");
  } catch {
    access = "";
  }
  if (!access) throw new DeliveryError("Google sign-in gave no access token.");
  const [project, dataset, table] = [config.project_id, config.dataset, config.table].map(String);
  const datasetUrl = `${BIGQUERY_URL()}/bigquery/v2/projects/${encodeURIComponent(project)}/datasets/${encodeURIComponent(dataset)}`;
  const auth = { authorization: `Bearer ${access}`, "content-type": "application/json" };
  const body = JSON.stringify({ skipInvalidRows: false, ignoreUnknownValues: false, rows: events.map((e) => ({ insertId: e.uuid, json: warehouseRow(e) })) });
  const insert = () => call(fetchImpl, `${datasetUrl}/tables/${encodeURIComponent(table)}/insertAll`, { method: "POST", headers: auth, body });
  let sent = await insert();
  if (sent.status === 404) {
    const created = await call(fetchImpl, `${datasetUrl}/tables`, {
      method: "POST",
      headers: auth,
      body: JSON.stringify({ tableReference: { projectId: project, datasetId: dataset, tableId: table }, schema: BQ_SCHEMA, timePartitioning: { type: "DAY", field: "timestamp" } }),
    });
    if (created.status !== 200 && created.status !== 409) fail("BigQuery", created);
    sent = await insert();
  }
  if (sent.status < 200 || sent.status >= 300) fail("BigQuery", sent);
  let insertErrors: unknown[] = [];
  try {
    insertErrors = (JSON.parse(sent.body).insertErrors as unknown[]) ?? [];
  } catch {
    insertErrors = [];
  }
  if (insertErrors.length > 0) throw new DeliveryError(`BigQuery refused ${insertErrors.length} row(s) of the batch.`, sent.status);
  return { bytes: Buffer.byteLength(body), status: sent.status };
}

/** Sends one batch to a warehouse destination. */
export async function sendWarehouse(kind: WarehouseKind, config: Record<string, unknown>, target: string, events: ExportedEvent[], now: Date, fetchImpl: typeof fetch): Promise<{ bytes: number; status: number | null }> {
  if (events.length === 0) return { bytes: 0, status: null };
  if (kind === "clickhouse") {
    let password = "";
    try {
      password = String(JSON.parse(target).password ?? "");
    } catch {
      throw new DeliveryError("The destination's secret could not be read on this deployment.");
    }
    return sendClickHouse(config, password, events, fetchImpl);
  }
  return sendBigQuery(config, target, events, now, fetchImpl);
}
