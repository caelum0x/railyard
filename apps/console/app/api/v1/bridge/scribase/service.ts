// The Scribase bridge (Wave P PY1.0, RYP-E6). Scribase sells Railyard inside
// its portfolio add-on; its control-api calls these three routes, server to
// server, signed with Standard Webhooks (a shared `whsec_` secret):
//
//   POST /api/v1/bridge/scribase/orgs    {scribase_org_id, name, addon}
//        Creates the Railyard org on first use, bound to the Scribase org id
//        (no second sign-up), and mirrors the add-on state into its
//        entitlement. Idempotent.
//   POST /api/v1/bridge/scribase/claims  {scribase_org_id, slot}
//        A one-time, pre-approved device code (two minutes). Scribase hands it
//        to its own CLI, which exchanges it once at /api/v1/device/token and
//        writes the token to the Railyard CLI's credentials file (mode 600).
//        The token is never in a response to Scribase, a terminal or a chat.
//   POST /api/v1/bridge/scribase/deploys {scribase_org_id, app?, branch?}
//        The org's deploys (newest first) for the Scribase Deploy tab (PY1.2).
//   POST /api/v1/bridge/scribase/proof   {scribase_org_id, slot}
//        {slot, state, live, proof:{kind,id,at}, next}: the real thing that
//        happened for this slot, or "arriving" while its package has not landed.
//
// Contract fixtures: railyard/tests/contract/fixtures/scribase-bridge/.

import { sha256Hex } from "../../../../../lib/crypto.ts";
import { access } from "../../../../../lib/ladder.ts";
import { listDeploys } from "../../deploys/service.ts";
import type { Repo } from "../../../../../lib/repo.ts";
import { verify, type WebhookHeaders } from "../../../../../lib/standard-webhooks.ts";
import { createApprovedDevice } from "../../../../../lib/tokens.ts";
import { emptyEntitlement, type Entitlement } from "../../../../../lib/types.ts";

export const CLAIM_TTL_SECONDS = 120;

/** Every slot of the portfolio add-on (dispatch section 5.2). */
export const ADDON_SLOTS = ["deploy", "analytics", "flags", "errors", "uptime", "ai", "billing", "consent"] as const;
export type AddonSlot = (typeof ADDON_SLOTS)[number];

/**
 * The slots Railyard serves through this bridge, the package that makes each
 * one live, and whether that package has landed. `errors` is Scribase's own
 * slot (SP-D6); the add-on only lifts its caps, so it is not served here.
 */
export const BRIDGE_SLOTS = {
  deploy: { pkg: "PY1.2", landed: true, proof: "deploy" },
  analytics: { pkg: "PY1.3", landed: false, proof: "first_event" },
  ai: { pkg: "PY1.4", landed: false, proof: "gateway_key" },
  flags: { pkg: "PY2.1", landed: false, proof: "flag_flip" },
  uptime: { pkg: "PY2.2", landed: false, proof: "check_result" },
  billing: { pkg: "PY2.3", landed: false, proof: "test_purchase" },
  consent: { pkg: "PY2.5", landed: false, proof: "consent_record" },
} as const;
export type BridgeSlot = keyof typeof BRIDGE_SLOTS;

export interface BridgeDeps {
  repo: Repo;
  secret: string;
  consoleUrl: string;
  now: () => Date;
}

export interface BridgeResult {
  status: number;
  body: Record<string, unknown>;
}

export interface SlotState {
  slot: string;
  state: "live" | "arriving" | "not_entitled" | "no_proof_yet";
  live: boolean;
  proof: { kind: string; id: string; at: string } | null;
  next: string | null;
}

const ORG_ID = /^[A-Za-z0-9_-]{1,128}$/;
const MAX_APPS = 1000;
const MAX_EVENTS = 1_000_000_000;
// Fixed namespace for the synthetic owner id of a bridged org (UUID v5).
const OWNER_NAMESPACE = "6f3b8a2e-4c1d-5e7f-9a0b-1c2d3e4f5a6b";

const fail = (status: number, error: string): BridgeResult => ({ status, body: { error } });

/** The deterministic owner id of the Railyard org bound to one Scribase org (UUID v5). */
export async function ownerIdFor(scribaseOrgId: string): Promise<string> {
  const ns = OWNER_NAMESPACE.replace(/-/g, "").match(/../g)!.map((h) => parseInt(h, 16));
  const name = new TextEncoder().encode(`scribase-org:${scribaseOrgId}`);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-1", new Uint8Array([...ns, ...name])));
  const b = digest.slice(0, 16);
  b[6] = (b[6] & 0x0f) | 0x50;
  b[8] = (b[8] & 0x3f) | 0x80;
  const hex = [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

interface AddonState {
  active: boolean;
  slots: AddonSlot[];
  renews_at: string | null;
  caps: { apps: number; analytics_events_month: number };
}

interface SyncInput {
  scribase_org_id: string;
  name: string;
  addon: AddonState;
}

const isInt = (v: unknown, max: number): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max;

function parseAddon(v: unknown): AddonState | string {
  if (!v || typeof v !== "object") return "addon must be an object";
  const a = v as Record<string, unknown>;
  if (typeof a.active !== "boolean") return "addon.active must be true or false";
  if (!Array.isArray(a.slots) || !a.slots.every((s) => (ADDON_SLOTS as readonly unknown[]).includes(s))) {
    return `addon.slots must list only: ${ADDON_SLOTS.join(", ")}`;
  }
  const renews = a.renews_at ?? null;
  if (renews !== null && (typeof renews !== "string" || Number.isNaN(Date.parse(renews)))) return "addon.renews_at must be a date or null";
  const caps = a.caps as Record<string, unknown> | undefined;
  if (!caps || !isInt(caps.apps, MAX_APPS) || !isInt(caps.analytics_events_month, MAX_EVENTS)) {
    return "addon.caps needs apps and analytics_events_month as whole numbers";
  }
  const slots = [...new Set(a.slots as AddonSlot[])].sort();
  return { active: a.active, slots, renews_at: renews as string | null, caps: { apps: caps.apps, analytics_events_month: caps.analytics_events_month } };
}

function parseOrgRef(b: Record<string, unknown>): string | null {
  return typeof b.scribase_org_id === "string" && ORG_ID.test(b.scribase_org_id) ? b.scribase_org_id : null;
}

function parseSync(body: Record<string, unknown>): SyncInput | string {
  const scribase_org_id = parseOrgRef(body);
  if (!scribase_org_id) return "scribase_org_id must be 1-128 letters, digits, dashes or underscores";
  const name = typeof body.name === "string" ? body.name.trim() : "";
  if (name.length < 1 || name.length > 80) return "name must be 1-80 characters";
  const addon = parseAddon(body.addon);
  if (typeof addon === "string") return addon;
  return { scribase_org_id, name, addon };
}

function withAddon(ent: Entitlement, addon: AddonState, now: Date): Entitlement {
  return {
    ...ent,
    plan: "addon",
    status: addon.active ? "active" : "inactive",
    paused: false,
    cancel_at_period_end: false,
    // Limits are kept when the add-on lapses: the org is paused, never emptied.
    apps_limit: addon.caps.apps,
    events_limit: addon.caps.analytics_events_month,
    addon_slots: addon.slots,
    addon_renews_at: addon.renews_at,
    updated_at: now.toISOString(),
  };
}

/** Creates or finds the bound org and mirrors the add-on into its entitlement. */
export async function syncOrg(repo: Repo, input: SyncInput, now: Date): Promise<BridgeResult> {
  const bound = await repo.findScribaseBinding(input.scribase_org_id);
  let orgId = bound?.org_id ?? null;
  const created = !orgId;
  if (!orgId) {
    const ownerId = await ownerIdFor(input.scribase_org_id);
    const org = await repo.createOrgForUser({
      userId: ownerId,
      // Not a mailbox: bridged orgs have no Railyard sign-in of their own.
      email: `scribase-org-${(await sha256Hex(input.scribase_org_id)).slice(0, 16)}@scribase.invalid`,
      name: input.name,
    });
    orgId = (await repo.bindScribaseOrg(input.scribase_org_id, org.id)).org_id;
  }
  const current = (await repo.getEntitlement(orgId)) ?? emptyEntitlement(orgId, now);
  const ent = await repo.saveEntitlement(withAddon(current, input.addon, now));
  return {
    status: 200,
    body: { org_id: orgId, created, addon: { active: ent.status === "active", slots: ent.addon_slots ?? [] } },
  };
}

async function entitledOrg(repo: Repo, scribaseOrgId: string, slot: string, now: Date): Promise<{ orgId: string; ownerId: string } | BridgeResult> {
  const bound = await repo.findScribaseBinding(scribaseOrgId);
  if (!bound) return fail(404, "This Scribase organization has no Railyard org yet. Turn on the add-on first.");
  const ent = await repo.getEntitlement(bound.org_id);
  const a = access(ent, now);
  if (a.state !== "active" || !(ent?.addon_slots ?? []).includes(slot)) {
    return fail(402, `The add-on is not active for ${slot}. Nothing was changed; your data is kept.`);
  }
  return { orgId: bound.org_id, ownerId: await ownerIdFor(scribaseOrgId) };
}

export async function issueClaim(deps: BridgeDeps, body: Record<string, unknown>): Promise<BridgeResult> {
  const scribaseOrgId = parseOrgRef(body);
  if (!scribaseOrgId) return fail(400, "scribase_org_id is required");
  const slot = body.slot;
  if (typeof slot !== "string" || !(slot in BRIDGE_SLOTS)) return fail(400, `slot must be one of: ${Object.keys(BRIDGE_SLOTS).join(", ")}`);
  const now = deps.now();
  const who = await entitledOrg(deps.repo, scribaseOrgId, slot, now);
  if ("status" in who) return who;
  const claim = await createApprovedDevice(deps.repo, who.orgId, who.ownerId, CLAIM_TTL_SECONDS, now);
  return {
    status: 200,
    body: { claim, expires_in: CLAIM_TTL_SECONDS, token_endpoint: `${deps.consoleUrl}/api/v1/device/token` },
  };
}

/** The proof for one slot of one org. `landed` overrides the table in tests of a later package. */
export async function slotProof(repo: Repo, orgId: string, slot: BridgeSlot, landed: Partial<Record<BridgeSlot, boolean>> = {}): Promise<SlotState> {
  const isLanded = landed[slot] ?? BRIDGE_SLOTS[slot].landed;
  if (!isLanded) {
    return { slot, state: "arriving", live: false, proof: null, next: `This slot is arriving (${BRIDGE_SLOTS[slot].pkg}); nothing was changed.` };
  }
  if (slot === "deploy") {
    const last = (await repo.listDeploys(orgId)).find((d) => d.status === "succeeded");
    return last
      ? { slot, state: "live", live: true, proof: { kind: "deploy", id: last.id, at: last.created_at }, next: null }
      : { slot, state: "no_proof_yet", live: true, proof: null, next: "Deploy from the project folder to see the first deploy here." };
  }
  return { slot, state: "no_proof_yet", live: true, proof: null, next: null };
}

async function proofFor(deps: BridgeDeps, body: Record<string, unknown>): Promise<BridgeResult> {
  const scribaseOrgId = parseOrgRef(body);
  if (!scribaseOrgId) return fail(400, "scribase_org_id is required");
  const slot = body.slot;
  if (typeof slot !== "string" || !(slot in BRIDGE_SLOTS)) return fail(400, `slot must be one of: ${Object.keys(BRIDGE_SLOTS).join(", ")}`);
  const s = slot as BridgeSlot;
  if (!BRIDGE_SLOTS[s].landed) return { status: 200, body: { ...(await slotProof(deps.repo, "", s)) } };
  const bound = await deps.repo.findScribaseBinding(scribaseOrgId);
  if (!bound) return { status: 200, body: { slot: s, state: "not_entitled", live: false, proof: null, next: "Turn on the add-on first." } };
  return { status: 200, body: { ...(await slotProof(deps.repo, bound.org_id, s)) } };
}

async function deploysFor(deps: BridgeDeps, body: Record<string, unknown>): Promise<BridgeResult> {
  const scribaseOrgId = parseOrgRef(body);
  if (!scribaseOrgId) return fail(400, "scribase_org_id is required");
  const str = (k: string, max: number) => (typeof body[k] === "string" && (body[k] as string).length <= max ? (body[k] as string) : null);
  const bound = await deps.repo.findScribaseBinding(scribaseOrgId);
  if (!bound) return { status: 200, body: { deploys: [] } };
  const r = await listDeploys(deps.repo, bound.org_id, str("app", 60), str("branch", 200));
  return { status: r.status, body: r.body };
}

export type BridgeRoute = "orgs" | "claims" | "proof" | "deploys";

/** Verify, refuse replays, parse, dispatch. Framework-free so the routes stay three lines. */
export async function handleBridge(deps: BridgeDeps, route: BridgeRoute, headers: WebhookHeaders, raw: string): Promise<BridgeResult> {
  const now = deps.now();
  if (!(await verify(deps.secret, headers, raw, Math.floor(now.getTime() / 1000)))) return fail(401, "invalid signature");
  let body: unknown;
  try {
    body = JSON.parse(raw);
  } catch {
    return fail(400, "body must be JSON");
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) return fail(400, "body must be a JSON object");
  if (!(await deps.repo.recordBridgeMessage(`scribase:${headers.id}`))) return fail(409, "this message was already processed");
  const b = body as Record<string, unknown>;
  switch (route) {
    case "orgs": {
      const input = parseSync(b);
      return typeof input === "string" ? fail(400, input) : syncOrg(deps.repo, input, now);
    }
    case "claims":
      return issueClaim(deps, b);
    case "proof":
      return proofFor(deps, b);
    case "deploys":
      return deploysFor(deps, b);
  }
}
