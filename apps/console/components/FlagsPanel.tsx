"use client";
import { useRouter } from "next/navigation";
import { useState } from "react";

export interface PanelFlag {
  key: string;
  enabled: boolean;
  value: string | null;
}

async function save(app: string, env: string, flag: Record<string, unknown>): Promise<string | null> {
  const res = await fetch("/api/flags", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ app, env, flag }) });
  if (res.ok) return null;
  const body = (await res.json().catch(() => ({}))) as { error?: string };
  return body.error ?? "Could not save the flag. Try again.";
}

/** One environment's flags: flip with one click, add one with a key (PY2.1). */
export function FlagsPanel({ app, env, flags }: { app: string; env: string; flags: PanelFlag[] }) {
  const router = useRouter();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const run = async (id: string, flag: Record<string, unknown>) => {
    setBusy(id);
    setError(null);
    const e = await save(app, env, flag);
    setBusy(null);
    if (e) setError(e);
    else router.refresh();
  };
  return (
    <>
      {flags.length ? (
        <ul className="list">
          {flags.map((f) => (
            <li key={f.key}>
              <span>
                <strong>{f.key}</strong> {f.value ? <code className="muted">{f.value}</code> : null}
              </span>
              <button type="button" className={f.enabled ? "primary" : undefined} aria-pressed={f.enabled} disabled={busy !== null} onClick={() => run(f.key, { key: f.key, enabled: !f.enabled })}>
                {busy === f.key ? "Saving" : f.enabled ? "On" : "Off"}
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">No flags here yet.</p>
      )}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) void run("new", { key: key.trim(), enabled: false }).then(() => setKey(""));
        }}
      >
        <input type="text" aria-label="New flag key" placeholder="new_checkout" value={key} onChange={(e) => setKey(e.target.value)} />
        <button type="submit" disabled={busy !== null || !key.trim()}>
          {busy === "new" ? "Adding" : "Add flag"}
        </button>
      </form>
      {error ? <p className="error">{error}</p> : null}
    </>
  );
}
