// The insights screens, under the product analytics frame.
import type { ReactNode } from "react";
import { Shell } from "../../_components/Shell.tsx";

const SCREENS = [
  { href: "/product-analytics/insights", label: "Insights" },
  { href: "/product-analytics/dashboards", label: "Dashboards" },
  { href: "/product-analytics/insights/segments", label: "Segments" },
  { href: "/product-analytics/actions", label: "Actions" },
  { href: "/product-analytics/sql", label: "SQL" },
];

export function InsightsShell({ title, names, app, current, children }: { title: string; names: string[]; app: string | null; current: string; children: ReactNode }) {
  const q = app ? `?app=${encodeURIComponent(app)}` : "";
  return (
    <Shell title={title} apps={names} app={app}>
      <nav className="row" aria-label="Insights" style={{ marginBottom: 16, fontSize: 14 }}>
        {SCREENS.map((s) => (
          <a key={s.href} href={`${s.href}${q}`} aria-current={current === s.href ? "page" : undefined} className={current === s.href ? undefined : "muted"}>
            {s.label}
          </a>
        ))}
      </nav>
      {children}
    </Shell>
  );
}

export function Notice({ text }: { text: string }) {
  return (
    <section className="card">
      <p>{text}</p>
    </section>
  );
}

export const NOT_CONNECTED = "Product analytics is arriving on this deployment.";
export const NO_APP = "Add an app first. Its insights appear here.";
