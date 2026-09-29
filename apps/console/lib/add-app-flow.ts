// The "Add app" flow on the Apps screen as a pure state machine. The React
// component (components/AddApp.tsx) renders these states, so the click count
// the acceptance test measures is the click count a user sees.

import type { Snippet } from "./apps.ts";

export type FlowState =
  | { step: "list" }
  | { step: "form"; name: string; error: string | null }
  | { step: "saving"; name: string }
  | { step: "done"; name: string; snippet: Snippet };

export type FlowAction =
  | { type: "click_add" }
  | { type: "type_name"; name: string }
  | { type: "click_create" }
  | { type: "created"; snippet: Snippet }
  | { type: "failed"; error: string }
  | { type: "click_close" };

/** Actions that are a click (typing is not a click). */
export const CLICKS: ReadonlySet<FlowAction["type"]> = new Set(["click_add", "click_create", "click_close"]);

export function addAppFlow(state: FlowState, action: FlowAction): FlowState {
  switch (action.type) {
    case "click_add":
      return state.step === "list" ? { step: "form", name: "", error: null } : state;
    case "type_name":
      return state.step === "form" ? { ...state, name: action.name, error: null } : state;
    case "click_create":
      return state.step === "form" && state.name.trim() ? { step: "saving", name: state.name } : state;
    case "created":
      return state.step === "saving" ? { step: "done", name: state.name, snippet: action.snippet } : state;
    case "failed":
      return state.step === "saving" ? { step: "form", name: state.name, error: action.error } : state;
    case "click_close":
      return { step: "list" };
  }
}

/** Drive the flow like a user would and count the clicks to the snippet. */
export async function clicksToSnippet(
  reducer: typeof addAppFlow,
  create: (name: string) => Promise<Snippet>,
  name: string,
): Promise<{ clicks: number; snippet: Snippet | null; state: FlowState }> {
  let clicks = 0;
  let state: FlowState = { step: "list" };
  const act = (a: FlowAction) => {
    if (CLICKS.has(a.type)) clicks += 1;
    state = reducer(state, a);
  };
  act({ type: "click_add" });
  act({ type: "type_name", name });
  act({ type: "click_create" });
  // Read through a function: `act` changes `state` in a closure.
  const now = (): FlowState => state;
  const saving = now();
  if (saving.step !== "saving") return { clicks, snippet: null, state: saving };
  try {
    act({ type: "created", snippet: await create(saving.name) });
  } catch (err) {
    act({ type: "failed", error: (err as Error).message });
  }
  const end = now();
  return { clicks, snippet: end.step === "done" ? end.snippet : null, state: end };
}
