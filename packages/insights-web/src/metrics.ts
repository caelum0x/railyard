/**
 * The five page-speed metrics of one page view (Wave P PY3.3), computed from
 * the browser's performance entries the way Chrome's own field reporting
 * defines them. Adapted from MIT-licensed source (see NOTICE), with two
 * changes so the numbers match Chrome's:
 *
 *  - LCP stops at the first user input: a larger paint after a click, tap or
 *    key press is not the page's LCP.
 *  - INP picks its percentile from the page's total interaction count
 *    (`performance.interactionCount` when the browser gives it), not only
 *    from the slow interactions that were observed.
 *
 * Pure: entries in, numbers out, so it is tested without a browser.
 */

export interface PerfEntry {
  entryType: string;
  name: string;
  startTime: number;
  duration: number;
  responseStart?: number;
  activationStart?: number;
  /** layout-shift */
  value?: number;
  hadRecentInput?: boolean;
  /** event / first-input */
  interactionId?: number;
}

export interface Vitals {
  lcp?: number;
  inp?: number;
  cls?: number;
  fcp?: number;
  ttfb?: number;
}

const INPUT_EVENTS = new Set(['pointerdown', 'pointerup', 'mousedown', 'click', 'keydown', 'keyup', 'touchstart']);

const round = (v: number, places = 0) => {
  const f = 10 ** places;
  return Math.round(v * f) / f;
};

/**
 * @param entries every entry observed for the page view (navigation, paint,
 *   largest-contentful-paint, layout-shift, event, first-input)
 * @param opts.interactionCount the page's interaction count when known
 * @param opts.layoutShiftSupported true where the browser reports layout
 *   shifts: a page with none has CLS 0, as Chrome reports it
 */
export function computeVitals(entries: readonly PerfEntry[], opts: { interactionCount?: number; layoutShiftSupported?: boolean } = {}): Vitals {
  const interactionCount = opts.interactionCount;
  const out: Vitals = {};
  const nav = entries.find((e) => e.entryType === 'navigation');
  const activationStart = nav?.activationStart ?? 0;
  if (nav && typeof nav.responseStart === 'number') out.ttfb = round(Math.max(nav.responseStart - activationStart, 0));

  const fcp = entries.find((e) => e.entryType === 'paint' && e.name === 'first-contentful-paint');
  if (fcp) out.fcp = round(Math.max(fcp.startTime - activationStart, 0));

  // LCP: the last candidate before the first input.
  const inputs = entries.filter((e) => (e.entryType === 'event' || e.entryType === 'first-input') && INPUT_EVENTS.has(e.name));
  const firstInput = inputs.length ? Math.min(...inputs.map((e) => e.startTime)) : Infinity;
  const lcps = entries.filter((e) => e.entryType === 'largest-contentful-paint' && e.startTime < firstInput).sort((a, b) => a.startTime - b.startTime);
  const lcp = lcps[lcps.length - 1];
  if (lcp) out.lcp = round(Math.max(lcp.startTime - activationStart, 0));

  // CLS: the worst session window (shifts under 1 s apart, a window at most 5 s long).
  let windowValue = 0;
  let windowFirst: PerfEntry | null = null;
  let windowLast: PerfEntry | null = null;
  let cls: number | undefined;
  for (const e of entries.filter((x) => x.entryType === 'layout-shift' && !x.hadRecentInput).sort((a, b) => a.startTime - b.startTime)) {
    if (windowFirst && windowLast && e.startTime - windowLast.startTime < 1000 && e.startTime - windowFirst.startTime < 5000) {
      windowValue += e.value ?? 0;
      windowLast = e;
    } else {
      windowValue = e.value ?? 0;
      windowFirst = e;
      windowLast = e;
    }
    cls = Math.max(cls ?? 0, windowValue);
  }
  if (cls !== undefined) out.cls = round(cls, 4);
  else if (opts.layoutShiftSupported) out.cls = 0;

  // INP: the longest duration per interaction, then one skipped per 50 interactions.
  const byInteraction = new Map<number, number>();
  for (const e of entries) {
    if ((e.entryType === 'event' || e.entryType === 'first-input') && e.interactionId) {
      byInteraction.set(e.interactionId, Math.max(byInteraction.get(e.interactionId) ?? 0, e.duration));
    }
  }
  const durations = [...byInteraction.values()].sort((a, b) => b - a);
  if (durations.length) {
    const count = Math.max(interactionCount ?? 0, durations.length);
    out.inp = round(durations[Math.min(Math.floor(count / 50), durations.length - 1)] as number);
  }
  return out;
}

/** The 75th percentile (linear, as the Insights p75 is computed). */
export function p75(values: readonly number[]): number | null {
  if (!values.length) return null;
  const s = [...values].sort((a, b) => a - b);
  const pos = 0.75 * (s.length - 1);
  const lo = Math.floor(pos);
  return (s[lo] as number) + ((s[Math.min(lo + 1, s.length - 1)] as number) - (s[lo] as number)) * (pos - lo);
}
