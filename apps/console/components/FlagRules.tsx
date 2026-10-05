"use client";
import { useRouter } from "next/navigation";
import { useState } from "react";

// Rules and % rollouts for one flag (QY1.3). One form: who (everyone, a
// property, a segment) and what share of them. The rules already saved are
// listed in order; the first that matches a person decides.

export interface RuleView {
  conditions: { type: string; key?: string; operator?: string; value?: unknown; id?: string }[];
  rollout: number;
  value?: string | null;
}

const OPERATORS = [
  { value: "exact", label: "is" },
  { value: "is_not", label: "is not" },
  { value: "icontains", label: "contains" },
  { value: "not_icontains", label: "does not contain" },
  { value: "regex", label: "matches pattern" },
  { value: "gt", label: "greater than" },
  { value: "gte", label: "at least" },
  { value: "lt", label: "less than" },
  { value: "lte", label: "at most" },
  { value: "is_set", label: "is set" },
  { value: "is_not_set", label: "is not set" },
];

function describe(r: RuleView): string {
  const who = r.conditions.length
    ? r.conditions
        .map((c) => (c.type === "segment" ? `segment ${c.key}` : c.type === "property" ? `${c.key} ${OPERATORS.find((o) => o.value === c.operator)?.label ?? c.operator}${c.value === undefined ? "" : ` ${Array.isArray(c.value) ? c.value.join(" or ") : String(c.value)}`}` : "a saved group"))
        .join(" and ")
    : "everyone";
  return `${who}: ${r.rollout}%${r.value !== undefined ? ` (value ${r.value ?? "none"})` : ""}`;
}

async function post(app: string, env: string, flag: Record<string, unknown>): Promise<string | null> {
  const res = await fetch("/api/flags", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ app, env, flag }) });
  if (res.ok) return null;
  const body = (await res.json().catch(() => ({}))) as { error?: string };
  return body.error ?? "Could not save the rules. Try again.";
}

export function FlagRules({
  app,
  env,
  flag,
  segments,
  groups,
}: {
  app: string;
  env: string;
  flag: { key: string; enabled: boolean; rules: RuleView[] | null };
  segments: string[];
  /** Saved groups of people from product analytics, offered only where that module is live. */
  groups: boolean;
}) {
  const router = useRouter();
  const [who, setWho] = useState<"everyone" | "property" | "segment" | "group">("everyone");
  const [prop, setProp] = useState("plan");
  const [op, setOp] = useState("exact");
  const [val, setVal] = useState("");
  const [segment, setSegment] = useState(segments[0] ?? "");
  const [group, setGroup] = useState("");
  const [rollout, setRollout] = useState("100");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (targeting: unknown) => {
    setBusy(true);
    setError(null);
    const e = await post(app, env, { key: flag.key, enabled: flag.enabled, targeting });
    setBusy(false);
    if (e) setError(e);
    else router.refresh();
  };
  const condition = () => {
    if (who === "property") return [{ type: "property", key: prop.trim(), operator: op, ...(op === "is_set" || op === "is_not_set" ? {} : { value: /^-?\d+(\.\d+)?$/.test(val.trim()) && op.startsWith("g") ? Number(val) : val.includes(",") && (op === "exact" || op === "is_not") ? val.split(",").map((v) => v.trim()).filter(Boolean) : val }) }];
    if (who === "segment") return [{ type: "segment", key: segment }];
    if (who === "group") return [{ type: "cohort", id: group.trim() }];
    return [];
  };
  return (
    <div className="stack" style={{ marginTop: 8 }}>
      {flag.rules?.length ? (
        <ol className="muted">
          {flag.rules.map((r, i) => (
            <li key={i}>{describe(r)}</li>
          ))}
        </ol>
      ) : (
        <p className="muted">Everyone gets this flag when it is on.</p>
      )}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          const rule = { conditions: condition(), rollout: Number(rollout) };
          void run({ rules: [...(flag.rules ?? []), rule] });
        }}
      >
        <select aria-label="Who" value={who} onChange={(e) => setWho(e.target.value as typeof who)}>
          <option value="everyone">Everyone</option>
          <option value="property">People whose property</option>
          {segments.length ? <option value="segment">A segment</option> : null}
          {groups ? <option value="group">Cohort</option> : null}
        </select>
        {who === "property" ? (
          <>
            <input type="text" aria-label="Property" value={prop} onChange={(e) => setProp(e.target.value)} />
            <select aria-label="Comparison" value={op} onChange={(e) => setOp(e.target.value)}>
              {OPERATORS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
            {op === "is_set" || op === "is_not_set" ? null : <input type="text" aria-label="Value (commas for several)" value={val} onChange={(e) => setVal(e.target.value)} />}
          </>
        ) : null}
        {who === "segment" ? (
          <select aria-label="Segment" value={segment} onChange={(e) => setSegment(e.target.value)}>
            {segments.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        ) : null}
        {who === "group" ? <input type="text" aria-label="Id" value={group} onChange={(e) => setGroup(e.target.value)} /> : null}
        <label>
          Share
          <input type="number" min={0} max={100} step={0.1} aria-label="Share of these people, in percent" value={rollout} onChange={(e) => setRollout(e.target.value)} style={{ width: 80 }} />%
        </label>
        <button type="submit" disabled={busy}>
          {busy ? "Saving" : "Add rule"}
        </button>
        {flag.rules?.length ? (
          <button type="button" disabled={busy} onClick={() => void run(null)}>
            Remove rules
          </button>
        ) : null}
      </form>
      {error ? <p className="error">{error}</p> : null}
    </div>
  );
}

/** The environment's segments: named sets of property conditions rules can use. */
export function FlagSegments({ app, env, segments }: { app: string; env: string; segments: { key: string; name: string; conditions: RuleView["conditions"] }[] }) {
  const router = useRouter();
  const [key, setKey] = useState("");
  const [prop, setProp] = useState("plan");
  const [op, setOp] = useState("exact");
  const [val, setVal] = useState("");
  const [error, setError] = useState<string | null>(null);
  const send = async (body: Record<string, unknown>) => {
    setError(null);
    const res = await fetch("/api/flags/segments", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ app, env, ...body }) });
    if (res.ok) router.refresh();
    else setError(((await res.json().catch(() => ({}))) as { error?: string }).error ?? "Could not save the segment. Try again.");
  };
  return (
    <div className="stack">
      <h3>Segments</h3>
      {segments.length ? (
        <ul className="list">
          {segments.map((s) => (
            <li key={s.key}>
              <span>
                <strong>{s.key}</strong> <span className="muted">{describe({ conditions: s.conditions, rollout: 100 }).replace(/: 100%$/, "")}</span>
              </span>
              <button type="button" onClick={() => void send({ remove: s.key })}>
                Remove
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">No segments yet. A segment names a group of people once, for many flags.</p>
      )}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          const value = val.includes(",") && (op === "exact" || op === "is_not") ? val.split(",").map((v) => v.trim()).filter(Boolean) : val;
          void send({ segment: { key: key.trim(), conditions: [{ type: "property", key: prop.trim(), operator: op, ...(op === "is_set" || op === "is_not_set" ? {} : { value }) }] } }).then(() => setKey(""));
        }}
      >
        <input type="text" aria-label="Segment key" placeholder="paying" value={key} onChange={(e) => setKey(e.target.value)} />
        <input type="text" aria-label="Property" value={prop} onChange={(e) => setProp(e.target.value)} />
        <select aria-label="Comparison" value={op} onChange={(e) => setOp(e.target.value)}>
          {OPERATORS.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <input type="text" aria-label="Value (commas for several)" value={val} onChange={(e) => setVal(e.target.value)} />
        <button type="submit" disabled={!key.trim()}>
          Add segment
        </button>
      </form>
      {error ? <p className="error">{error}</p> : null}
    </div>
  );
}
