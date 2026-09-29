"use client";
import { useReducer, useState } from "react";
import { addAppFlow, type FlowState } from "../lib/add-app-flow.ts";
import type { Snippet } from "../lib/apps.ts";
import { CloseIcon, CopyIcon } from "./Icons.tsx";

export function AddAppView({
  state,
  remaining,
  onAdd,
  onName,
  onCreate,
  onClose,
  onCopy,
  copied,
}: {
  state: FlowState;
  remaining: number;
  onAdd: () => void;
  onName: (name: string) => void;
  onCreate: () => void;
  onClose: () => void;
  onCopy: (text: string) => void;
  copied: string | null;
}) {
  if (state.step === "list") {
    return (
      <div className="row">
        <button className="primary" type="button" onClick={onAdd} disabled={remaining <= 0}>
          Add app
        </button>
        <span className="muted">{remaining > 0 ? `${remaining} left on your plan` : "Your plan's apps are all in use"}</span>
      </div>
    );
  }
  if (state.step === "form" || state.step === "saving") {
    return (
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          onCreate();
        }}
      >
        <input type="text" autoFocus aria-label="App name" placeholder="my-app" value={state.name} onChange={(e) => onName(e.target.value)} />
        <button className="primary" type="submit" disabled={state.step === "saving"}>
          {state.step === "saving" ? "Creating" : "Create"}
        </button>
        <button className="icon" type="button" aria-label="Close" title="Close" onClick={onClose}>
          <CloseIcon />
        </button>
        {state.step === "form" && state.error ? <p className="error">{state.error}</p> : null}
      </form>
    );
  }
  const block = (label: string, text: string) => (
    <div style={{ marginTop: 14 }}>
      <div className="row" style={{ justifyContent: "space-between" }}>
        <h2>{label}</h2>
        <button className="icon" type="button" aria-label={`Copy ${label.toLowerCase()}`} title="Copy" onClick={() => onCopy(text)}>
          <CopyIcon />
        </button>
      </div>
      <pre>{text}</pre>
      {copied === text ? <span className="muted">Copied</span> : null}
    </div>
  );
  return (
    <div data-testid="snippet">
      <div className="row" style={{ justifyContent: "space-between" }}>
        <strong>{state.name} is ready</strong>
        <button className="icon" type="button" aria-label="Close" title="Close" onClick={onClose}>
          <CloseIcon />
        </button>
      </div>
      {block("Deploy", state.snippet.deploy)}
      {block("Add to your page", state.snippet.script)}
    </div>
  );
}

export function AddApp({ remaining }: { remaining: number }) {
  const [state, dispatch] = useReducer(addAppFlow, { step: "list" } as FlowState);
  const [copied, setCopied] = useState<string | null>(null);
  async function create() {
    if (state.step !== "form" || !state.name.trim()) return;
    const name = state.name;
    dispatch({ type: "click_create" });
    try {
      const res = await fetch("/api/apps", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name }) });
      const body = (await res.json()) as { snippet?: Snippet; error?: string };
      if (!res.ok || !body.snippet) dispatch({ type: "failed", error: body.error ?? "Could not add the app." });
      else dispatch({ type: "created", snippet: body.snippet });
    } catch {
      dispatch({ type: "failed", error: "Could not reach Railyard. Check your connection." });
    }
  }
  return (
    <AddAppView
      state={state}
      remaining={remaining}
      copied={copied}
      onAdd={() => dispatch({ type: "click_add" })}
      onName={(name) => dispatch({ type: "type_name", name })}
      onCreate={create}
      onClose={() => {
        dispatch({ type: "click_close" });
        if (state.step === "done") window.location.reload();
      }}
      onCopy={(text) => {
        navigator.clipboard?.writeText(text).then(() => setCopied(text), () => setCopied(null));
      }}
    />
  );
}
