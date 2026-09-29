"use client";
import { useRouter } from "next/navigation";
import { useState } from "react";

export interface PanelCheck {
  id: string;
  app: string;
  name: string;
  url: string;
  paused: boolean;
  state: string;
  status_url: string;
  last_checked_at: string | null;
}

async function post(body: Record<string, unknown>): Promise<string | null> {
  const res = await fetch("/api/watch", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  if (res.ok) return null;
  return ((await res.json().catch(() => ({}))) as { error?: string }).error ?? "Could not save. Try again.";
}

const LABEL: Record<string, string> = { up: "Up", down: "Down", waiting: "Waiting" };

/** Uptime checks: add one by address, pause or resume, open the status page (PY2.2). */
export function WatchPanel({ apps, checks }: { apps: string[]; checks: PanelCheck[] }) {
  const router = useRouter();
  const [app, setApp] = useState(apps[0] ?? "");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (body: Record<string, unknown>) => {
    setBusy(true);
    setError(null);
    const e = await post(body);
    setBusy(false);
    if (e) setError(e);
    else {
      setUrl("");
      router.refresh();
    }
  };
  return (
    <>
      {checks.length ? (
        <ul className="list">
          {checks.map((c) => (
            <li key={c.id}>
              <span>
                <strong>{c.name}</strong> <span className="muted">{c.url}</span>
              </span>
              <span className="row">
                <span className={c.state === "down" ? "error" : c.state === "up" ? "ok" : "muted"}>{c.paused ? "Paused" : LABEL[c.state] ?? c.state}</span>
                <a href={c.status_url}>Status page</a>
                <button type="button" disabled={busy} onClick={() => run({ id: c.id, paused: !c.paused })}>
                  {c.paused ? "Resume" : "Pause"}
                </button>
              </span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">No checks yet. Add your app's address; we check it every minute.</p>
      )}
      {apps.length ? (
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            void run({ app, url });
          }}
        >
          <select aria-label="App" value={app} onChange={(e) => setApp(e.target.value)}>
            {apps.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
          <input type="url" aria-label="Address to check" placeholder="https://your-app.example.com" value={url} onChange={(e) => setUrl(e.target.value)} />
          <button className="primary" type="submit" disabled={busy || !url.trim()}>
            {busy ? "Adding" : "Add check"}
          </button>
        </form>
      ) : (
        <p className="muted">Add an app first.</p>
      )}
      {error ? <p className="error">{error}</p> : null}
    </>
  );
}
