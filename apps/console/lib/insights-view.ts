// The Insights page as a pure model: per-app rows and the three-step funnel.
import { capMessage } from "./caps.ts";
import type { InsightsSummary } from "./insights.ts";

export interface InsightsView {
  apps: { name: string; firstEvent: string | null; visitors: number; events: number }[];
  steps: { label: string; sessions: number; ofPrevious: string | null }[];
  empty: boolean;
  /** At or near the plan cap: exactly what the edge collector does (lib/caps.ts). */
  capNote: string | null;
}

const LABELS: Record<string, string> = { visit: "Visited", signup: "Signed up", first_action: "Did a first action" };

export function insightsView(summary: InsightsSummary, names: Map<string, string>, eventsLimit: number | null = null, now = new Date()): InsightsView {
  const apps = summary.apps.map((a) => ({
    name: names.get(a.app_id) ?? a.app_id.slice(0, 8),
    firstEvent: a.first_event_at ? `${a.first_event_at.slice(0, 16).replace("T", " ")} UTC` : null,
    visitors: a.visitors,
    events: a.events,
  }));
  const steps = summary.funnel.steps.map((s, i, all) => {
    const prev = i === 0 ? null : all[i - 1].sessions;
    return { label: LABELS[s.name] ?? s.name, sessions: s.sessions, ofPrevious: prev ? `${Math.round((s.sessions / prev) * 100)}%` : null };
  });
  const monthEvents = summary.apps.reduce((sum, a) => sum + (a.month_events ?? 0), 0);
  const capNote = eventsLimit === null ? null : capMessage(monthEvents, eventsLimit, now);
  return { apps, steps, empty: apps.every((a) => a.events === 0), capNote };
}
