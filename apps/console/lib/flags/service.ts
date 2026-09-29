// Railyard Flags service (Wave P PY2.1): environments, flags and the public
// evaluation the SDK polls. Every write names its org; a client key reads one
// environment only (tenant isolation, PY2.1 acceptance 3).

import { randomToken } from "../crypto.ts";
import type { FlagsStore } from "./store.ts";
import { FlagsBackendError } from "./flagsmith-backend.ts";
import {
  CLIENT_KEY,
  ENV_NAME,
  FLAG_KEY,
  MAX_DESCRIPTION,
  MAX_FLAGS_PER_ENV,
  MAX_VALUE,
  type BackendName,
  type Flag,
  type FlagEnv,
  type FlagInput,
  type FlagsBackend,
} from "./types.ts";

export const MAX_ENVS_PER_ORG = 500;
/** How long an evaluation may be cached (the SDK polls every 30 s: a flip shows within 60 s). */
export const EVAL_MAX_AGE_SECONDS = 15;

export interface FlagsDeps {
  store: FlagsStore;
  /** The backend new environments use. */
  backend: FlagsBackend;
  /** Every backend this deployment can serve (an env keeps the backend it was made on). */
  backends: Partial<Record<BackendName, FlagsBackend>>;
}

export class FlagsError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

const APP = /^[a-z0-9][a-z0-9-]{0,39}$/;

export function newClientKey(): string {
  return `ry_flags_${randomToken(24)}`;
}

function backendFor(deps: FlagsDeps, env: FlagEnv): FlagsBackend {
  const b = deps.backends[env.backend];
  if (!b) throw new FlagsError(503, "Flags are being set up on this deployment. Nothing was changed.");
  return b;
}

async function wrap<T>(work: () => Promise<T>): Promise<T> {
  try {
    return await work();
  } catch (e) {
    if (e instanceof FlagsBackendError) {
      console.error(`flags backend: ${e.message}`);
      throw new FlagsError(502, "The flags service did not answer. Nothing was changed; try again shortly.");
    }
    throw e;
  }
}

/** Validates a flag write from any caller (console, bridge). */
export function parseFlagInput(raw: unknown): FlagInput | string {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return "the flag must be an object";
  const b = raw as Record<string, unknown>;
  if (typeof b.key !== "string" || !FLAG_KEY.test(b.key)) return "key must start with a letter and use lowercase letters, digits and underscores (64 at most)";
  if (typeof b.enabled !== "boolean") return "enabled must be true or false";
  const out: FlagInput = { key: b.key, enabled: b.enabled };
  if (b.value !== undefined) {
    if (b.value !== null && (typeof b.value !== "string" || b.value.length > MAX_VALUE)) return `value must be text of at most ${MAX_VALUE} characters, or null`;
    out.value = b.value as string | null;
  }
  if (b.description !== undefined) {
    if (b.description !== null && (typeof b.description !== "string" || b.description.length > MAX_DESCRIPTION)) return `description must be at most ${MAX_DESCRIPTION} characters`;
    out.description = b.description as string | null;
  }
  return out;
}

/** The environment's row, created (and seeded from `copyFrom`) on first use. */
export async function ensureEnv(
  deps: FlagsDeps,
  input: { orgId: string; orgName: string; app: string; env: string; copyFrom?: string | null },
  now: Date,
): Promise<{ env: FlagEnv; created: boolean }> {
  if (!APP.test(input.app)) throw new FlagsError(400, "app must be a project name (lowercase letters, digits, dashes)");
  if (!ENV_NAME.test(input.env)) throw new FlagsError(400, "env must be an environment or branch name");
  const existing = await deps.store.findEnv(input.orgId, input.app, input.env);
  if (existing) return { env: existing, created: false };
  const siblings = await deps.store.listEnvs(input.orgId);
  if (siblings.length >= MAX_ENVS_PER_ORG) throw new FlagsError(409, `An organization can have ${MAX_ENVS_PER_ORG} flag environments. Remove old branches first.`);
  const refs = await wrap(() => deps.backend.prepare({ org_id: input.orgId, app: input.app, env: input.env }, input.orgName, siblings));
  const parent = input.copyFrom && input.copyFrom !== input.env ? siblings.find((e) => e.app === input.app && e.env === input.copyFrom) ?? null : null;
  const env = await deps.store.createEnv({
    org_id: input.orgId,
    app: input.app,
    env: input.env,
    client_key: newClientKey(),
    backend: deps.backend.name,
    ...refs,
    copied_from: parent?.id ?? null,
  });
  if (parent) {
    // A branch starts with its parent's flags, then changes on its own.
    const flags = await wrap(() => backendFor(deps, parent).list(parent));
    for (const f of flags) {
      await wrap(() => backendFor(deps, env).set(env, { key: f.key, enabled: f.enabled, value: f.value, description: f.description }, "branch-copy", now));
    }
  }
  return { env, created: true };
}

export function listFlags(deps: FlagsDeps, env: FlagEnv): Promise<Flag[]> {
  return wrap(() => backendFor(deps, env).list(env));
}

/** Creates or changes one flag of one environment. */
export async function setFlag(deps: FlagsDeps, env: FlagEnv, raw: unknown, actor: string, now: Date): Promise<Flag> {
  const input = parseFlagInput(raw);
  if (typeof input === "string") throw new FlagsError(400, input);
  const backend = backendFor(deps, env);
  const current = await wrap(() => backend.list(env));
  if (!current.some((f) => f.key === input.key) && current.length >= MAX_FLAGS_PER_ENV) {
    throw new FlagsError(409, `An environment can have ${MAX_FLAGS_PER_ENV} flags.`);
  }
  return wrap(() => backend.set(env, input, actor, now));
}

export interface Evaluation {
  env: string;
  flags: Record<string, { enabled: boolean; value: string | null }>;
  fetched_at: string;
}

/** What an app reads with its public client key. Records the environment's first read (the slot's proof). */
export async function evaluate(deps: FlagsDeps, clientKey: string | null, now: Date): Promise<Evaluation> {
  if (!clientKey || !CLIENT_KEY.test(clientKey)) throw new FlagsError(401, "A valid flags key is required.");
  const env = await deps.store.findEnvByClientKey(clientKey);
  if (!env) throw new FlagsError(401, "A valid flags key is required.");
  const flags = await listFlags(deps, env);
  if (!env.first_read_at) await deps.store.markFirstRead(env.id, now.toISOString());
  return {
    env: env.env,
    flags: Object.fromEntries(flags.map((f) => [f.key, { enabled: f.enabled, value: f.value }])),
    fetched_at: now.toISOString(),
  };
}

/** The flags slot's proof: the first read of any environment's flags by an app. */
export async function firstRead(store: FlagsStore, orgId: string): Promise<{ kind: string; id: string; at: string } | null> {
  const read = (await store.listEnvs(orgId)).filter((e) => e.first_read_at).sort((a, b) => (a.first_read_at! < b.first_read_at! ? -1 : 1))[0];
  return read ? { kind: "first_flag_read", id: `${read.app}/${read.env}`, at: read.first_read_at! } : null;
}
