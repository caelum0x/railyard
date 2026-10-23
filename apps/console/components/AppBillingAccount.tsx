"use client";
// Mint self-serve account provisioning (backlog item 11): shows the app's
// billing ledger account id once minted, or a one-click "provision" action
// when there is none yet — replaces the operator curling Mint's admin API.

import { useState } from "react";
import { CopyIcon, WalletIcon } from "./Icons.tsx";

export function AppBillingAccount({ appId, mintAccountId }: { appId: string; mintAccountId: string | null }) {
  const [id, setId] = useState(mintAccountId);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  async function provision() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(`/api/apps/${appId}/mint-account`, { method: "POST" });
      const body = (await res.json()) as { mint_account_id?: string; error?: string };
      if (!res.ok || !body.mint_account_id) setError(body.error ?? "Could not provision a billing account.");
      else setId(body.mint_account_id);
    } catch {
      setError("Could not reach Railyard. Check your connection.");
    } finally {
      setBusy(false);
    }
  }

  if (id) {
    return (
      <span className="row" style={{ gap: 6 }}>
        <WalletIcon />
        <code className="muted" title="Mint billing account id -- paste into this app's RAILYARD_MINT_ACCOUNT_ID / mint_account_id setting">
          {id}
        </code>
        <button
          className="icon"
          type="button"
          aria-label="Copy billing account id"
          title="Copy"
          onClick={() => navigator.clipboard?.writeText(id).then(() => setCopied(true), () => setCopied(false))}
        >
          <CopyIcon />
        </button>
        {copied ? <span className="muted">Copied</span> : null}
      </span>
    );
  }
  return (
    <span className="row" style={{ gap: 6 }}>
      <button className="icon" type="button" aria-label="Provision billing account" title="Create a Mint billing account for this app" onClick={provision} disabled={busy}>
        <WalletIcon />
      </button>
      <span className="muted">{busy ? "Provisioning" : "No billing account yet"}</span>
      {error ? <span className="error">{error}</span> : null}
    </span>
  );
}
