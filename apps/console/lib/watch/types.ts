// Railyard Watch (Wave P PY2.2, RYP-E4): uptime checks and public status pages.
//
// A check is one public https URL of one app. The customer checks run on a
// Gatus instance of their own on box 1 (never the one behind our own status
// page); an agent next to it pulls the list of checks from this console and
// pushes each result back. The console keeps the latest results, flips the
// state after two failures in a row, emails the owner through Rally Send on
// each change, and renders /status/<slug> from live results only.

export const CHECK_NAME = /^[\p{L}\p{N}][\p{L}\p{N} ._'-]{0,59}$/u;
export const SLUG = /^[a-z0-9][a-z0-9-]{2,60}$/;
export const HISTORY = 90;
export const FAILS_TO_DOWN = 2;
export const MAX_CHECKS_PER_ORG = 20;

export type CheckState = "waiting" | "up" | "down";

export interface Check {
  id: string;
  org_id: string;
  app: string;
  name: string;
  url: string;
  slug: string;
  owner_email: string | null;
  paused: boolean;
  created_at: string;
}

export interface ResultPoint {
  at: string;
  ok: boolean;
  ms: number;
}

export interface CheckStatus {
  check_id: string;
  state: CheckState;
  consecutive_failures: number;
  first_result_at: string | null;
  last_checked_at: string | null;
  last_change_at: string | null;
  /** The state the owner was last emailed about. */
  alerted_state: CheckState | null;
  history: ResultPoint[];
}

export interface AgentResult {
  id: string;
  ok: boolean;
  at: string;
  ms: number;
}
