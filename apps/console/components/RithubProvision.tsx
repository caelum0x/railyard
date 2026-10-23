"use client";
// Rithub self-serve provisioning (backlog item 4): one button on the public
// /rithub landing page creates a dedicated Gitea org + first repo for the
// signed-in Console session, then shows the clone command -- no separate
// native Gitea signup.

import { useState } from "react";
import { CopyIcon, GitRepoIcon } from "./Icons.tsx";

interface Provisioned {
  username: string;
  org_slug: string;
  repo_url: string;
}

export function RithubProvision() {
  const [result, setResult] = useState<Provisioned | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  async function create() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch("/api/v1/rithub/provision", { method: "POST" });
      const body = (await res.json()) as Provisioned & { error?: string };
      if (!res.ok || !body.repo_url) setError(body.error ?? "Could not create your repo.");
      else setResult(body);
    } catch {
      setError("Could not reach Railyard. Check your connection.");
    } finally {
      setBusy(false);
    }
  }

  if (result) {
    const clone = `git clone ${result.repo_url}.git`;
    return (
      <div style={{ display: "grid", gap: 8, justifyContent: "center" }}>
        <p className="muted">
          Your repo is ready under <strong>{result.org_slug}</strong>.
        </p>
        <span className="row" style={{ gap: 6, justifyContent: "center" }}>
          <code>{clone}</code>
          <button
            className="icon"
            type="button"
            aria-label="Copy clone command"
            title="Copy"
            onClick={() => navigator.clipboard?.writeText(clone).then(() => setCopied(true), () => setCopied(false))}
          >
            <CopyIcon />
          </button>
          {copied ? <span className="muted">Copied</span> : null}
        </span>
        <a className="ghost" style={{ justifyContent: "center" }} href={result.repo_url} target="_blank" rel="noopener noreferrer">
          Open on Rithub &rarr;
        </a>
      </div>
    );
  }
  return (
    <div style={{ display: "grid", gap: 8, justifyContent: "center" }}>
      <button className="primary" style={{ justifyContent: "center" }} type="button" onClick={create} disabled={busy}>
        <GitRepoIcon /> {busy ? "Creating" : "Create your repo"}
      </button>
      {error ? <span className="error">{error}</span> : null}
    </div>
  );
}
