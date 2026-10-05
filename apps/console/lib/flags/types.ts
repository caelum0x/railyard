// Railyard Flags (Wave P PY2.1, RYP-E2, SP-E6): row shapes and the backend seam.
//
// A flag environment is one app environment of one org (a Scribase project
// environment or branch: app = project, env = environment id). Its client key
// is public (it ships in the app, like an anon key) and reads that one
// environment's flags, nothing else. Two backends serve the same surface:
//   flagsmith  the unmodified Flagsmith server on box 2 (default when configured)
//   table      Railyard's own tables (migration 0005), the fallback when the
//              box 2 memory gate keeps Flagsmith down.

import type { Segment, Targeting } from "./targeting.mjs";

export type { Condition, Rule, Segment, Targeting, TargetingContext } from "./targeting.mjs";

export const FLAG_KEY = /^[a-z][a-z0-9_]{0,63}$/;
export const ENV_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
export const CLIENT_KEY = /^ry_flags_[A-Za-z0-9_-]{24,64}$/;
export const MAX_VALUE = 2000;
export const MAX_DESCRIPTION = 200;
export const MAX_FLAGS_PER_ENV = 500;

export type BackendName = "table" | "flagsmith";

export interface FlagEnv {
  id: string;
  org_id: string;
  app: string;
  env: string;
  client_key: string;
  backend: BackendName;
  /** Flagsmith: the environment's API key (server side only; never sent to a browser). */
  backend_ref: string | null;
  /** Flagsmith: the project id the environment belongs to. */
  backend_project: string | null;
  /** Flagsmith: the organisation id of this Railyard org (one per org: tenant isolation). */
  backend_org: string | null;
  copied_from: string | null;
  first_read_at: string | null;
  created_at: string;
}

export interface Flag {
  key: string;
  enabled: boolean;
  value: string | null;
  description: string | null;
  updated_at: string | null;
  /** Rules and % rollouts (QY1.3); null for a plain on/off flag. */
  targeting?: Targeting | null;
}

export interface FlagInput {
  key: string;
  enabled: boolean;
  value?: string | null;
  description?: string | null;
  /** Absent: unchanged. Null: the flag is plain again. */
  targeting?: Targeting | null;
}

export type SegmentRow = Segment & { updated_at: string | null };

export type BackendRefs = Pick<FlagEnv, "backend_ref" | "backend_project" | "backend_org">;

/** Where flag values live. Envs and their client keys always live in Railyard's store. */
export interface FlagsBackend {
  readonly name: BackendName;
  /**
   * Creates the backend side of a new environment (a no-op for the table
   * backend). `siblings` are the org's existing environments, so an app keeps
   * one backend project and an org one backend organisation.
   */
  prepare(input: { org_id: string; app: string; env: string }, orgName: string, siblings: FlagEnv[]): Promise<BackendRefs>;
  list(env: FlagEnv): Promise<Flag[]>;
  /** Creates the flag when missing, then sets it. Returns the flag as stored. */
  set(env: FlagEnv, input: FlagInput, actor: string, now: Date): Promise<Flag>;
}
