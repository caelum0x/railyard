// The destination catalogue (QY1.4, A23): Slack, Discord and HubSpot next to
// the signed webhook and the S3 bucket. Each kind turns a batch of the
// project's events into its target's request; every request also carries the
// Standard Webhooks headers (webhook-id, webhook-timestamp, webhook-signature)
// under the destination's own signing secret, so a receiving proxy can verify
// it. Delivery, retries with backoff, the run log and the on/off switch are
// the exports runner's (runner.ts), the same for every kind.
//
// Message shapes follow the destination templates of the open-source product
// analytics CDP (MIT; nodejs/src/cdp/templates/_destinations/{slack,hubspot},
// commit 1348173f5911; nothing from ee/): Slack gets {text, blocks}, Discord
// {content}, HubSpot contacts are upserted by email through its public
// CRM v3 batch upsert API with the customer's private app token.

import { sign } from "../bridge/standard-webhooks.ts";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { allowPrivate, checkUrlShape, DestinationRefused } from "./net-guard.ts";
import { DeliveryError, SEND_TIMEOUT_MS, type ExportedEvent } from "./runner.ts";
import { newSigningSecret } from "./secrets.ts";
import { checkWarehouse, describeWarehouse, isWarehouse, sendWarehouse } from "./warehouse.ts";

export const CATALOGUE = ["slack", "discord", "hubspot", "clickhouse", "bigquery"] as const;
export type CatalogueKind = (typeof CATALOGUE)[number];
export const isCatalogue = (k: string): k is CatalogueKind => (CATALOGUE as readonly string[]).includes(k);

/** Lines per Slack or Discord message; the rest are counted. */
export const MESSAGE_LINES = 20;
export const DISCORD_MAX = 2000;
export const HUBSPOT_BATCH = 100;
export const HUBSPOT_URL = () => (process.env.PA_HUBSPOT_URL?.trim() || "https://api.hubapi.com").replace(/\/$/, "");
/** HubSpot contact properties copied from the person's $set (standard properties only). */
export const HUBSPOT_PROPERTIES = ["firstname", "lastname", "phone", "company", "website", "jobtitle", "city", "country"] as const;

/** What the sealed secret of a catalogue destination holds. */
export interface SecretBundle {
  target: string;
  signing: string;
}

export const sealBundle = (b: SecretBundle): string => JSON.stringify(b);

export function openBundle(raw: string): SecretBundle {
  const v = JSON.parse(raw) as Partial<SecretBundle>;
  if (typeof v.target !== "string" || typeof v.signing !== "string") throw new Error("destination secret has an unknown format");
  return { target: v.target, signing: v.signing };
}

function hostOnly(u: URL, hosts: string[], path: RegExp, what: string): void {
  if (allowPrivate()) return; // local development and tests point at a local fake
  if (!hosts.includes(u.hostname) || !path.test(u.pathname)) throw new ApiError(400, `${what} must be an address ${hosts[0]} gave you.`);
}

/** Validates a catalogue destination's input. Returns what is kept in the clear and the sealed bundle. */
export function checkCatalogue(kind: CatalogueKind, c: Record<string, unknown>, secretIn: unknown): { config: Record<string, string>; bundle: SecretBundle } {
  const signing = newSigningSecret();
  if (isWarehouse(kind)) {
    const { config, target } = checkWarehouse(kind, c, secretIn);
    return { config, bundle: { target, signing } };
  }
  const guard = (raw: string, what: string) => {
    try {
      return checkUrlShape(raw, what);
    } catch (err) {
      if (err instanceof DestinationRefused) throw new ApiError(400, err.message);
      throw err;
    }
  };
  if (kind === "slack") {
    const u = guard(text(secretIn, "The Slack webhook address", 500)!, "The Slack webhook address");
    hostOnly(u, ["hooks.slack.com"], /^\/services\//, "The Slack webhook address");
    return { config: { label: text(c.label, "The channel label", 80, false) ?? "Slack" }, bundle: { target: u.toString(), signing } };
  }
  if (kind === "discord") {
    const u = guard(text(secretIn, "The Discord webhook address", 500)!, "The Discord webhook address");
    hostOnly(u, ["discord.com", "discordapp.com"], /^\/api\/webhooks\//, "The Discord webhook address");
    return { config: { label: text(c.label, "The channel label", 80, false) ?? "Discord" }, bundle: { target: u.toString(), signing } };
  }
  const token = text(secretIn, "The HubSpot private app token", 500)!;
  if (!/^[A-Za-z0-9_-]{10,500}$/.test(token)) throw new ApiError(400, "The HubSpot private app token is not in a known form.");
  return { config: { label: text(c.label, "The account label", 80, false) ?? "HubSpot" }, bundle: { target: token, signing } };
}

const when = (iso: string) => iso.replace("T", " ").slice(0, 16) + " UTC";
const where = (e: ExportedEvent) => (typeof e.properties.$current_url === "string" ? ` on ${e.properties.$current_url}` : typeof e.properties.$screen_name === "string" ? ` on ${e.properties.$screen_name}` : "");

/** One line per event, at most MESSAGE_LINES, then a count. */
export function eventLines(events: ExportedEvent[]): string[] {
  const lines = events.slice(0, MESSAGE_LINES).map((e) => `${e.event} by ${e.distinct_id}${where(e)} at ${when(e.timestamp)}`);
  if (events.length > MESSAGE_LINES) lines.push(`and ${events.length - MESSAGE_LINES} more`);
  return lines;
}

export function slackBody(events: ExportedEvent[]): { text: string; blocks: unknown[] } {
  const lines = eventLines(events);
  const head = `${events.length} new event${events.length === 1 ? "" : "s"}`;
  return {
    text: `${head}: ${lines[0] ?? ""}`,
    blocks: [
      { type: "section", text: { type: "mrkdwn", text: `*${head}*` } },
      { type: "section", text: { type: "mrkdwn", text: lines.map((l) => `- ${l}`).join("\n").slice(0, 2900) } },
    ],
  };
}

export function discordBody(events: ExportedEvent[]): { content: string; allowed_mentions: { parse: never[] } } {
  const lines = eventLines(events);
  let content = `${events.length} new event${events.length === 1 ? "" : "s"}\n${lines.map((l) => `- ${l}`).join("\n")}`;
  if (content.length > DISCORD_MAX) content = `${content.slice(0, DISCORD_MAX - 1)}…`;
  // No @everyone or role pings from event data.
  return { content, allowed_mentions: { parse: [] } };
}

const EMAIL = /^[^\s@]{1,64}@[^\s@]{1,253}\.[^\s@]{2,63}$/;

/** HubSpot contact upserts from the events that name a person's email (the latest values win). */
export function hubspotInputs(events: ExportedEvent[]): { idProperty: "email"; id: string; properties: Record<string, string> }[] {
  const byEmail = new Map<string, Record<string, string>>();
  for (const e of events) {
    const set = (e.properties.$set && typeof e.properties.$set === "object" ? e.properties.$set : {}) as Record<string, unknown>;
    const raw = set.email ?? e.properties.email;
    if (typeof raw !== "string" || !EMAIL.test(raw.trim())) continue;
    const email = raw.trim().toLowerCase();
    const props: Record<string, string> = { ...(byEmail.get(email) ?? {}), email };
    for (const k of HUBSPOT_PROPERTIES) if (typeof set[k] === "string" || typeof set[k] === "number") props[k] = String(set[k]).slice(0, 500);
    byEmail.set(email, props);
  }
  return [...byEmail.entries()].map(([id, properties]) => ({ idProperty: "email" as const, id, properties }));
}

async function post(url: string, body: string, headers: Record<string, string>, fetchImpl: typeof fetch): Promise<number> {
  let res: Response;
  try {
    res = await fetchImpl(url, { method: "POST", redirect: "manual", signal: AbortSignal.timeout(SEND_TIMEOUT_MS), headers: { "content-type": "application/json", "user-agent": "railyard-product-analytics-export/1", ...headers }, body });
  } catch (err) {
    if (err instanceof DestinationRefused) throw new DeliveryError(err.message.slice(0, 300));
    throw new DeliveryError(`The destination could not be reached: ${(err as Error).message}`.slice(0, 300));
  }
  await res.arrayBuffer().catch(() => undefined);
  if (res.status < 200 || res.status >= 300) throw new DeliveryError(`The destination answered ${res.status}.`, res.status);
  return res.status;
}

/** Sends one batch to a catalogue destination. Returns the bytes sent (0 when nothing applied). */
export async function sendCatalogue(
  kind: CatalogueKind,
  bundle: SecretBundle,
  events: ExportedEvent[],
  messageId: string,
  now: Date,
  fetchImpl: typeof fetch,
  config: Record<string, unknown> = {},
): Promise<{ bytes: number; status: number | null }> {
  if (isWarehouse(kind)) return sendWarehouse(kind, config, bundle.target, events, now, fetchImpl);
  const ts = String(Math.floor(now.getTime() / 1000));
  const signed = (body: string) => ({ "webhook-id": messageId, "webhook-timestamp": ts, "webhook-signature": sign(bundle.signing, messageId, ts, body) });
  if (kind === "hubspot") {
    const inputs = hubspotInputs(events);
    if (inputs.length === 0) return { bytes: 0, status: null };
    const url = new URL(`${HUBSPOT_URL()}/crm/v3/objects/contacts/batch/upsert`);
    let bytes = 0;
    let status: number | null = null;
    for (let i = 0; i < inputs.length; i += HUBSPOT_BATCH) {
      const body = JSON.stringify({ inputs: inputs.slice(i, i + HUBSPOT_BATCH) });
      bytes += Buffer.byteLength(body);
      status = await post(url.toString(), body, { authorization: `Bearer ${bundle.target}`, ...signed(body) }, fetchImpl);
    }
    return { bytes, status };
  }
  const url = checkUrlShape(bundle.target, "The webhook address");
  const body = JSON.stringify(kind === "slack" ? slackBody(events) : discordBody(events));
  return { bytes: Buffer.byteLength(body), status: await post(url.toString(), body, signed(body), fetchImpl) };
}

/** The clear-text view of where a catalogue destination sends (never the secret). */
export function describeTarget(kind: CatalogueKind, config: Record<string, unknown>): string {
  if (isWarehouse(kind)) return describeWarehouse(kind, config);
  return `${kind === "slack" ? "Slack" : kind === "discord" ? "Discord" : "HubSpot"}: ${String(config.label ?? "")}`;
}
