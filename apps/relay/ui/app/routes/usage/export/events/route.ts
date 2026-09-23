/**
 * Server-side per-request usage export.
 *
 * Streams every durable cost event (captured or metered) in the requested
 * date range as CSV by paging through the gateway's cursor-paginated
 * `GET /v1/billing/cost-events` endpoint, so exports are not bounded by the
 * dashboard's summary window.
 */
import type { Route } from "./+types/route";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { scopeFromUrl } from "~/components/billing/BillingShared";
import { costEventState, isMeteredEvent } from "~/types/billing";
import type { CostEventRecord } from "~/types/billing";

const PAGE_SIZE = 100;
/** Hard ceiling on exported rows so one request cannot scan unbounded history. */
const MAX_EXPORT_ROWS = 250_000;
const MICROS_PER_USD = 1_000_000;
const DAY_MS = 24 * 60 * 60 * 1000;

const HEADERS = [
  "event_id",
  "request_id",
  "recorded_at",
  "api_key",
  "model",
  "route",
  "input_tokens",
  "output_tokens",
  "total_tokens",
  "charged_usd",
  "authorized_usd",
  "mode",
  "delivery_state",
  "delivery_attempts",
];

function parseDay(raw: string | null): number | null {
  if (!raw || !/^\d{4}-\d{2}-\d{2}$/.test(raw)) {
    return null;
  }
  const ms = Date.parse(`${raw}T00:00:00Z`);
  return Number.isNaN(ms) ? null : ms;
}

function escapeCsv(field: string | number): string {
  const text = String(field);
  return /[",\n\r]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function usd(micros: number): string {
  return (micros / MICROS_PER_USD).toFixed(6);
}

function toRow(record: CostEventRecord): string {
  const { event } = record;
  return [
    event.event_id,
    event.request_id,
    new Date(event.settled_at_ms).toISOString(),
    event.owner_principal,
    event.model,
    event.route,
    event.usage.input_tokens,
    event.usage.output_tokens,
    event.usage.total_tokens,
    usd(event.charged_micros),
    usd(event.authorized_micros),
    isMeteredEvent(event) ? "metered" : "mint_settled",
    costEventState(record),
    record.attempts,
  ]
    .map(escapeCsv)
    .join(",");
}

export async function loader({ request }: Route.LoaderArgs) {
  await requireValidApiKeyIfEnabled();
  const url = new URL(request.url);
  const scope = scopeFromUrl(url);
  const fromMs = parseDay(url.searchParams.get("from"));
  const toDayMs = parseDay(url.searchParams.get("to"));
  const toMs = toDayMs == null ? null : toDayMs + DAY_MS;
  if (fromMs != null && toMs != null && fromMs >= toMs) {
    return new Response("`from` must be on or before `to`", { status: 400 });
  }
  const client = getTensorZeroClient();
  // Fetch the first page eagerly so gateway/auth errors surface as a real HTTP
  // error instead of a truncated download.
  const first = await client.listCostEvents({ limit: PAGE_SIZE, scope });
  const encoder = new TextEncoder();

  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      controller.enqueue(encoder.encode(`${HEADERS.join(",")}\r\n`));
      let page = first;
      let written = 0;
      try {
        for (;;) {
          let reachedStart = false;
          for (const record of page.items) {
            const at = record.event.settled_at_ms;
            if (toMs != null && at >= toMs) {
              continue;
            }
            if (fromMs != null && at < fromMs) {
              reachedStart = true;
              continue;
            }
            controller.enqueue(encoder.encode(`${toRow(record)}\r\n`));
            written += 1;
            if (written >= MAX_EXPORT_ROWS) {
              reachedStart = true;
              break;
            }
          }
          // Events are ordered newest-first by time-ordered id; once a full
          // page falls before the range start, older pages cannot match.
          const pageAllBefore =
            fromMs != null &&
            page.items.length > 0 &&
            page.items.every((record) => record.event.settled_at_ms < fromMs);
          if (!page.next_cursor || (reachedStart && pageAllBefore) || written >= MAX_EXPORT_ROWS) {
            break;
          }
          page = await client.listCostEvents({
            limit: PAGE_SIZE,
            before: page.next_cursor,
            scope,
          });
        }
        controller.close();
      } catch (error) {
        controller.error(error);
      }
    },
  });

  const label = `${url.searchParams.get("from") ?? "start"}_to_${url.searchParams.get("to") ?? "now"}`;
  return new Response(stream, {
    headers: {
      "Content-Type": "text/csv; charset=utf-8",
      "Content-Disposition": `attachment; filename="relay_cost_events_${label.replace(/[^0-9A-Za-z_-]/g, "")}.csv"`,
      "Cache-Control": "no-store",
    },
  });
}
