// Builder form fields. Plain HTML controls in a GET form, so a built
// insight is a link that can be shared and reloaded.
import type { Form, Meta } from "../../_lib/insights-client.ts";

const label = { display: "grid", gap: 4, fontSize: 13 } as const;

export function EventSelect({ name, value, meta, blank, title }: { name: string; value?: string; meta: Meta; blank?: string; title: string }) {
  return (
    <label style={label}>
      <span className="muted">{title}</span>
      <select name={name} defaultValue={value ?? ""}>
        {blank !== undefined ? <option value="">{blank}</option> : null}
        <option value="$all">Any event</option>
        {meta.events.map((e) => (
          <option key={e.event} value={e.event}>
            {e.event}
          </option>
        ))}
        {value && value !== "$all" && !value.startsWith("action:") && !meta.events.some((e) => e.event === value) ? <option value={value}>{value}</option> : null}
        {meta.actions.length ? (
          <optgroup label="Actions">
            {meta.actions.map((a) => (
              <option key={a.id} value={`action:${a.id}`}>
                {a.name}
              </option>
            ))}
          </optgroup>
        ) : null}
      </select>
    </label>
  );
}

export function Choice({ name, value, title, options }: { name: string; value?: string; title: string; options: { value: string; label: string }[] }) {
  return (
    <label style={label}>
      <span className="muted">{title}</span>
      <select name={name} defaultValue={value ?? options[0]?.value}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

export function Text({ name, value, title, placeholder }: { name: string; value?: string; title: string; placeholder?: string }) {
  return (
    <label style={label}>
      <span className="muted">{title}</span>
      <input type="text" name={name} defaultValue={value ?? ""} placeholder={placeholder} style={{ minWidth: 160 }} />
    </label>
  );
}

export const MATHS = [
  { value: "total", label: "Total count" },
  { value: "dau", label: "Unique people" },
  { value: "weekly_active", label: "Weekly active people" },
  { value: "monthly_active", label: "Monthly active people" },
  { value: "sum", label: "Sum of a property" },
  { value: "avg", label: "Average of a property" },
  { value: "min", label: "Minimum of a property" },
  { value: "max", label: "Maximum of a property" },
  { value: "median", label: "Median of a property" },
  { value: "p90", label: "90th percentile of a property" },
  { value: "p95", label: "95th percentile of a property" },
  { value: "p99", label: "99th percentile of a property" },
];

export function breakdownOptions(meta: Meta) {
  return [
    { value: "", label: "No breakdown" },
    ...meta.event_properties.slice(0, 100).map((k) => ({ value: `event:${k}`, label: `Event: ${k}` })),
    ...meta.person_properties.slice(0, 100).map((k) => ({ value: `person:${k}`, label: `Person: ${k}` })),
  ];
}

export function KindFields({ f, meta }: { f: Form; meta: Meta }) {
  const kind = f.kind ?? "trends";
  const row = { display: "flex", gap: 12, flexWrap: "wrap", alignItems: "end", marginBottom: 12 } as const;
  if (kind === "trends" || kind === "stickiness") {
    return (
      <>
        {[1, 2, 3].map((i) => (
          <div key={i} style={row}>
            <EventSelect name={`e${i}`} value={f[`e${i}`] ?? (i === 1 ? "$pageview" : undefined)} meta={meta} blank={i === 1 ? undefined : "None"} title={`Series ${String.fromCharCode(64 + i)}`} />
            {kind === "trends" ? <Choice name={`m${i}`} value={f[`m${i}`]} title="Measure" options={MATHS} /> : null}
            {kind === "trends" ? <Text name={`p${i}`} value={f[`p${i}`]} title="Numeric property" placeholder="amount" /> : null}
          </div>
        ))}
        {kind === "trends" ? (
          <div style={row}>
            <Choice name="breakdown" value={f.breakdown} title="Break down by" options={breakdownOptions(meta)} />
            <Text name="formula" value={f.formula} title="Formula" placeholder="A / B * 100" />
            <label style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13 }}>
              <input type="checkbox" name="compare" defaultChecked={f.compare === "on"} /> Compare to the period before
            </label>
          </div>
        ) : null}
      </>
    );
  }
  if (kind === "funnels") {
    return (
      <>
        <div style={row}>
          {[1, 2, 3, 4, 5].map((i) => (
            <EventSelect key={i} name={`e${i}`} value={f[`e${i}`] ?? (i === 1 ? "$pageview" : i === 2 ? "signup" : undefined)} meta={meta} blank={i <= 2 ? undefined : "None"} title={`Step ${i}`} />
          ))}
        </div>
        <div style={row}>
          <Choice name="order" value={f.order} title="Step order" options={[{ value: "ordered", label: "In order" }, { value: "strict", label: "In order, nothing between" }, { value: "unordered", label: "Any order" }]} />
          <Text name="window" value={f.window ?? "14"} title="Converted within" />
          <Choice name="wunit" value={f.wunit ?? "day"} title="Unit" options={["minute", "hour", "day", "week"].map((u) => ({ value: u, label: `${u}s` }))} />
          <Choice name="breakdown" value={f.breakdown} title="Break down by" options={breakdownOptions(meta)} />
        </div>
      </>
    );
  }
  if (kind === "retention") {
    return (
      <div style={row}>
        <EventSelect name="e1" value={f.e1 ?? "signup"} meta={meta} title="Start event" />
        <EventSelect name="e2" value={f.e2 ?? "$pageview"} meta={meta} title="Came back with" />
        <Choice name="period" value={f.period ?? "week"} title="Period" options={[{ value: "day", label: "Days" }, { value: "week", label: "Weeks" }, { value: "month", label: "Months" }]} />
        <Text name="intervals" value={f.intervals ?? "8"} title="Periods shown" />
        <Choice name="rtype" value={f.rtype} title="Rows start from" options={[{ value: "first_time", label: "The first time" }, { value: "recurring", label: "Every time" }]} />
      </div>
    );
  }
  if (kind === "paths") {
    return (
      <div style={row}>
        <Choice name="ptype" value={f.ptype} title="Steps are" options={[{ value: "pageview", label: "Page views" }, { value: "screen", label: "Screens" }, { value: "custom", label: "Custom events" }, { value: "all", label: "All of them" }]} />
        <Text name="start" value={f.start} title="Starting at" placeholder="https://example.com/" />
        <Text name="end" value={f.end} title="Ending at" />
        <Text name="depth" value={f.depth ?? "5"} title="Steps" />
      </div>
    );
  }
  return (
    <div style={row}>
      <EventSelect name="e1" value={f.e1 ?? "$pageview"} meta={meta} title="Active means" />
    </div>
  );
}
