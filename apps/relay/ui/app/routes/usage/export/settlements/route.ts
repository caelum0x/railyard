/**
 * Server-side settlement export.
 *
 * Streams every signed Mint settlement receipt in the requested date range as
 * CSV by paging through the gateway's keyset-paginated
 * `GET /relay/billing/settlements?before=` endpoint, so exports are not bounded
 * by the dashboard's summary window.
 */
import type { Route } from "./+types/route";
import { requireValidApiKeyIfEnabled } from "~/utils/auth.server";
import { getTensorZeroClient } from "~/utils/tensorzero.server";
import { scopeFromUrl } from "~/components/billing/BillingShared";
import type { SettlementDetail } from "~/types/billing";

const PAGE_SIZE = 500;
/** Hard ceiling on exported rows so one request cannot scan unbounded history. */
const MAX_EXPORT_ROWS = 250_000;
const MICROS_PER_USD = 1_000_000;
const DAY_MS = 24 * 60 * 60 * 1000;

const HEADERS = [
  "request_id",
  "settled_at",
  "amount_usd",
  "verified_at",
  "attested",
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

function toRow(settlement: SettlementDetail): string {
  return [
    settlement.request_id,
    new Date(settlement.settled_at_ms).toISOString(),
    (settlement.amount_micros / MICROS_PER_USD).toFixed(6),
    settlement.verified_at_ms != null
      ? new Date(settlement.verified_at_ms).toISOString()
      : "",
    settlement.has_attestation ? "yes" : "no",
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
  const first = await client.getBillingSettlements({
    settlements: PAGE_SIZE,
    scope,
  });
  const encoder = new TextEncoder();

  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      controller.enqueue(encoder.encode(`${HEADERS.join(",")}\r\n`));
      let page = first;
      let written = 0;
      try {
        for (;;) {
          let passedStart = false;
          for (const settlement of page.settlements) {
            const at = settlement.settled_at_ms;
            if (toMs != null && at >= toMs) {
              continue;
            }
            if (fromMs != null && at < fromMs) {
              // Newest-first ordering: everything after this is older still.
              passedStart = true;
              break;
            }
            controller.enqueue(encoder.encode(`${toRow(settlement)}\r\n`));
            written += 1;
            if (written >= MAX_EXPORT_ROWS) {
              break;
            }
          }
          if (passedStart || !page.next_before || written >= MAX_EXPORT_ROWS) {
            break;
          }
          page = await client.getBillingSettlements({
            settlements: PAGE_SIZE,
            scope,
            before: page.next_before,
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
      "Content-Disposition": `attachment; filename="relay_settlements_${label.replace(/[^0-9A-Za-z_-]/g, "")}.csv"`,
      "Cache-Control": "no-store",
    },
  });
}
