// The frame of the recordings, click maps, surveys, experiments and exports
// screens, inside the product analytics frame.
import type { ReactNode } from "react";
import { Shell } from "../../_components/Shell.tsx";
import { liveModules, type LiveModules } from "./live.ts";
import { PATHS } from "../../_lib/a3-client.ts";

// Each screen names the module switch it waits for (null: always shown).
export const A3_SCREENS: { href: string; label: string; needs: keyof LiveModules | null }[] = [
  { href: PATHS.replay, label: "Recordings", needs: "replay" },
  { href: PATHS.heat, label: "Click maps", needs: "replay" },
  { href: PATHS.surveys, label: "Surveys", needs: null },
  // The label of a flag-based screen is shown only once the flags slot is live.
  { href: PATHS.exp, label: "Variant tests", needs: "flags" },
  { href: PATHS.exports, label: "Exports", needs: null },
  { href: PATHS.alerts, label: "Alerts", needs: null },
];

export function A3Shell({ title, names, app, current, children, live = liveModules() }: { title: string; names: string[]; app: string | null; current: string; children: ReactNode; live?: LiveModules }) {
  const q = app ? `?app=${encodeURIComponent(app)}` : "";
  return (
    <Shell title={title} apps={names} app={app}>
      <nav className="row" aria-label="More screens" style={{ marginBottom: 16, fontSize: 14 }}>
        {A3_SCREENS.filter((s) => !s.needs || live[s.needs]).map((s) => (
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

export function Problem({ text }: { text: string | null | undefined }) {
  return text ? <p className="error">{text}</p> : null;
}

/** A labelled bar for a share of a total (tables stay the source of the numbers). */
export function ShareBar({ label, value, total }: { label: string; value: number; total: number }) {
  const share = total > 0 ? value / total : 0;
  return (
    <div style={{ display: "grid", gridTemplateColumns: "minmax(120px, 30%) 1fr 90px", gap: 8, alignItems: "center", fontSize: 14 }}>
      <span>{label}</span>
      <span aria-hidden="true" style={{ background: "#e6e9ef", borderRadius: 4, height: 12 }}>
        <span style={{ display: "block", width: `${Math.round(share * 100)}%`, background: "#2f5d9e", height: 12, borderRadius: 4 }} />
      </span>
      <span className="muted" style={{ textAlign: "right" }}>
        {value.toLocaleString("en-US")} ({total > 0 ? Math.round(share * 100) : 0}%)
      </span>
    </div>
  );
}

export const NOT_CONNECTED = "Product analytics is arriving on this deployment.";
export const NO_APP = "Add an app first. What it records appears here.";
export const fmt = (iso: string | null | undefined): string => (iso ? new Date(iso).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "");
