import { describe, expect, it } from "vitest";
import { traceListQuery } from "~/types/traces";
import {
  DEFAULT_WINDOW_HOURS,
  filtersFromUrl,
  formatLatency,
  formatTraceCost,
  hrefForFilters,
  traceStatus,
} from "./format";

describe("trace view formatting", () => {
  it("shows unknown prices honestly", () => {
    expect(formatTraceCost(undefined)).toBe("unpriced");
    expect(formatTraceCost(1250)).toBe("$0.00125");
  });

  it("formats latency", () => {
    expect(formatLatency(undefined)).toBe("-");
    expect(formatLatency(812)).toBe("812 ms");
    expect(formatLatency(2500)).toBe("2.50 s");
  });

  it("derives a status", () => {
    expect(traceStatus({ model_calls: 0 })).toBe("no model call stored");
    expect(traceStatus({ model_calls: 2, finish_reason: "stop" })).toBe("stop");
    expect(traceStatus({ model_calls: 1 })).toBe("unknown");
  });
});

describe("trace filters", () => {
  it("drops the key filter outside the operator scope", () => {
    const filters = filtersFromUrl("http://ui/observability/keys?key=abc");
    expect(filters.scope).toBe("own");
    expect(filters.key).toBeUndefined();
  });

  it("keeps operator filters and rejects unknown windows", () => {
    const filters = filtersFromUrl(
      "http://ui/observability/keys?scope=all&key=abc&window_hours=999",
    );
    expect(filters).toMatchObject({
      scope: "all",
      key: "abc",
      windowHours: DEFAULT_WINDOW_HOURS,
    });
  });

  it("round-trips filters through a URL", () => {
    const href = hrefForFilters("/observability/keys", {
      scope: "all",
      key: "k1",
      windowHours: 24,
      before: "1:abc",
      functionName: "draft_email",
    });
    expect(filtersFromUrl(`http://ui${href}`)).toEqual({
      scope: "all",
      key: "k1",
      windowHours: 24,
      before: "1:abc",
      functionName: "draft_email",
    });
  });

  it("builds the gateway query", () => {
    expect(traceListQuery()).toBe("");
    expect(traceListQuery({ scope: "own", key: "ignored", limit: 50 })).toBe(
      "?limit=50",
    );
    expect(
      traceListQuery({ scope: "all", key: "k", windowHours: 24, before: "c" }),
    ).toBe("?scope=all&key=k&window_hours=24&before=c");
  });
});
