// The Flagsmith backend (Wave P PY2.1): the unmodified Flagsmith server on
// box 2 (BSD-3-Clause) holds the flag values. Railyard maps
//   Railyard org  -> one Flagsmith organisation (tenant isolation)
//   app           -> one Flagsmith project
//   environment   -> one Flagsmith environment (its API key stays server side)
//   flag          -> a feature, and its state in that environment
// and talks to the admin REST API with the instance admin's token
// (FLAGS_ADMIN_TOKEN, `Authorization: Token ...`). Nothing here reaches a
// browser: apps read flags through Railyard's client endpoint with their
// public client key. The engine name never appears in customer copy.

import type { BackendRefs, Flag, FlagEnv, FlagInput, FlagsBackend } from "./types.ts";

type Fetch = typeof fetch;

export class FlagsBackendError extends Error {}

interface FsFeature {
  id: number;
  name: string;
  description?: string | null;
}
interface FsState {
  id: number;
  enabled: boolean;
  feature: number | { id: number; name: string };
  feature_state_value: string | number | boolean | null;
  updated_at?: string | null;
}
interface FsFlag {
  enabled: boolean;
  feature_state_value: string | number | boolean | null;
  feature: { id: number; name: string; description?: string | null };
}

const text = (v: FsState["feature_state_value"]): string | null => (v === null || v === undefined || v === "" ? null : String(v));

export class FlagsmithBackend implements FlagsBackend {
  readonly name = "flagsmith" as const;
  readonly base: string;
  private readonly opts: { apiUrl: string; adminToken: string; fetchImpl?: Fetch };

  constructor(opts: { apiUrl: string; adminToken: string; fetchImpl?: Fetch }) {
    this.opts = opts;
    this.base = `${opts.apiUrl.replace(/\/$/, "")}/api/v1`;
  }

  private async call<T>(method: string, path: string, body?: unknown, envKey?: string): Promise<T> {
    const headers: Record<string, string> = { accept: "application/json", "content-type": "application/json" };
    if (envKey) headers["x-environment-key"] = envKey;
    else headers.authorization = `Token ${this.opts.adminToken}`;
    const res = await (this.opts.fetchImpl ?? fetch)(`${this.base}${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    if (!res.ok) throw new FlagsBackendError(`flags service ${method} ${path.split("?")[0].replace(/[A-Za-z0-9]{20,}/g, "…")} answered ${res.status}`);
    const raw = await res.text();
    return (raw ? JSON.parse(raw) : null) as T;
  }

  async prepare(input: { org_id: string; app: string; env: string }, orgName: string, siblings: FlagEnv[]): Promise<BackendRefs> {
    const sameApp = siblings.find((e) => e.app === input.app && e.backend === "flagsmith" && e.backend_project && e.backend_org);
    let org = sameApp?.backend_org ?? siblings.find((e) => e.backend === "flagsmith" && e.backend_org)?.backend_org ?? null;
    if (!org) org = String((await this.call<{ id: number }>("POST", "/organisations/", { name: `${orgName} (${input.org_id.slice(0, 8)})` })).id);
    let project = sameApp?.backend_project ?? null;
    if (!project) project = String((await this.call<{ id: number }>("POST", "/projects/", { name: input.app, organisation: Number(org) })).id);
    const env = await this.call<{ id: number; api_key: string }>("POST", "/environments/", { name: input.env, project: Number(project) });
    if (!env?.api_key) throw new FlagsBackendError("flags service did not return an environment key");
    return { backend_ref: env.api_key, backend_project: project, backend_org: org };
  }

  private refs(env: FlagEnv): { key: string; project: string } {
    if (!env.backend_ref || !env.backend_project) throw new FlagsBackendError("this flag environment has no flags service reference");
    return { key: env.backend_ref, project: env.backend_project };
  }

  async list(env: FlagEnv): Promise<Flag[]> {
    const { key } = this.refs(env);
    const flags = await this.call<FsFlag[]>("GET", "/flags/", undefined, key);
    return (flags ?? [])
      .map((f) => ({ key: f.feature.name, enabled: f.enabled, value: text(f.feature_state_value), description: f.feature.description ?? null, updated_at: null }))
      .sort((a, b) => a.key.localeCompare(b.key));
  }

  private async feature(project: string, input: FlagInput): Promise<FsFeature> {
    const found = await this.call<{ results: FsFeature[] }>("GET", `/projects/${project}/features/?search=${encodeURIComponent(input.key)}&page_size=100`);
    const hit = (found?.results ?? []).find((f) => f.name === input.key);
    if (hit) return hit;
    return this.call<FsFeature>("POST", `/projects/${project}/features/`, {
      name: input.key,
      description: input.description ?? "",
      default_enabled: false,
      initial_value: null,
    });
  }

  async set(env: FlagEnv, input: FlagInput, _actor: string, now: Date): Promise<Flag> {
    const { key, project } = this.refs(env);
    const feature = await this.feature(project, input);
    const states = await this.call<{ results: FsState[] }>("GET", `/environments/${key}/featurestates/?feature=${feature.id}`);
    const state = (states?.results ?? [])[0];
    if (!state) throw new FlagsBackendError("flags service has no state for this flag in the environment");
    const patch: Record<string, unknown> = { enabled: input.enabled };
    if (input.value !== undefined) patch.feature_state_value = input.value;
    const saved = await this.call<FsState>("PATCH", `/environments/${key}/featurestates/${state.id}/`, patch);
    return {
      key: input.key,
      enabled: saved?.enabled ?? input.enabled,
      value: text(saved?.feature_state_value ?? (input.value === undefined ? state.feature_state_value : input.value)),
      description: input.description ?? feature.description ?? null,
      updated_at: now.toISOString(),
    };
  }
}
