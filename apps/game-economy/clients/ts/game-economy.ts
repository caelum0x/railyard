/**
 * game-economy TypeScript client (zero dependencies, uses global `fetch`).
 *
 * Works in Expo / React Native, browsers, Node 18+, Workers. Copy this file
 * into a game (e.g. `lib/railyard/game-economy.ts`); there is no npm package.
 *
 * Two halves, never mixed:
 *  - `GameEconomyPlayer`  ships IN the app. It only holds a short-lived player
 *    token issued by your game backend.
 *  - `GameEconomyServer`  runs ONLY on your game backend (it holds
 *    GAME_SERVER_SECRET). Never bundle it into an app.
 *
 * Every response is the envelope `{ success: true, data }` or
 * `{ success: false, error: { code, message } }`; failures throw
 * `GameEconomyError` with the server's stable `code`.
 * Money is integer microdollars (1 USD = 1_000_000).
 */

export type Micros = number;

export interface CatalogItem {
  sku: string;
  price_micros: Micros;
  kind: "card_pack" | "cosmetic" | "battle_pass";
  name: string;
  details: unknown;
}

export interface SettlementProof {
  sequence: number;
  operation: string;
  operation_hash: string;
  receipt_hash: string;
  signature: string;
}

export interface PurchaseOutcome {
  entitlement_id: string;
  product_sku: string;
  charged_micros: Micros;
  receipt_id: string;
  settlement_seq: number;
  settlement_proof: SettlementProof;
  idempotent_replay: boolean;
}

export interface GrantedItem {
  slot: number;
  rarity: string;
  name: string;
  item_id: string;
}

export interface Inventory {
  packs: Record<
    string,
    {
      entitlement_id: string;
      pack_sku: string;
      receipt_id: string;
      settlement_seq: number;
      contents: GrantedItem[] | null;
    }
  >;
  cosmetics: Record<string, string>;
  battle_pass: { pass_sku: string; tier: number } | null;
}

export interface Wallet {
  player: string;
  /** Spendable now. */
  available_micros: Micros;
  /** Reserved by purchases still in flight. */
  held_micros: Micros;
  inventory: Inventory;
}

export interface PurchaseRecord extends PurchaseOutcome {
  player: string;
  status: "completed" | "refunded" | "revoked";
  reversal: unknown;
}

export interface IapCredit {
  player: string;
  platform: string;
  transaction_id: string;
  product_id: string;
  direction: "credit" | "reverse";
  amount_micros: Micros;
  settlement_seq: number;
  receipt_hash: string | null;
  idempotent_replay: boolean;
}

export class GameEconomyError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "GameEconomyError";
  }
}

type Headers = Record<string, string>;

async function call<T>(
  baseUrl: string,
  path: string,
  init: { method?: string; headers?: Headers; body?: unknown; timeoutMs?: number },
): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), init.timeoutMs ?? 15_000);
  let res: Response;
  try {
    res = await fetch(`${baseUrl.replace(/\/+$/, "")}${path}`, {
      method: init.method ?? "GET",
      headers: {
        ...(init.body !== undefined ? { "content-type": "application/json" } : {}),
        ...init.headers,
      },
      body: init.body !== undefined ? JSON.stringify(init.body) : undefined,
      signal: controller.signal,
    });
  } catch (err) {
    const aborted = err instanceof Error && err.name === "AbortError";
    throw new GameEconomyError(0, aborted ? "timeout" : "network_error",
      aborted ? "The store did not respond in time." : "Could not reach the store.");
  } finally {
    clearTimeout(timer);
  }
  let payload: any = null;
  try {
    payload = await res.json();
  } catch {
    // Non-JSON body (proxy error page); handled below.
  }
  if (res.ok && payload?.success) return payload.data as T;
  const code = payload?.error?.code ?? `http_${res.status}`;
  const message = payload?.error?.message ?? `Store request failed (${res.status}).`;
  throw new GameEconomyError(res.status, code, message);
}

/** A random idempotency key; persist it with the pending purchase so a retry reuses it. */
export function newIdempotencyKey(): string {
  const c = (globalThis as any).crypto;
  if (c?.randomUUID) return c.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}

/** In-app client. `getToken` returns the current player token from your backend. */
export class GameEconomyPlayer {
  constructor(
    private readonly baseUrl: string,
    private readonly player: string,
    private readonly getToken: () => Promise<string> | string,
  ) {}

  private async auth(): Promise<Headers> {
    return { authorization: `Bearer ${await this.getToken()}` };
  }

  catalog(): Promise<{ products: CatalogItem[]; pools: unknown }> {
    return call(this.baseUrl, "/v1/catalog", {});
  }

  /**
   * Buy a SKU with the player's Mint balance. Reuse the same `idempotencyKey`
   * when retrying after a network error: the server returns the original
   * result and never charges twice.
   */
  async purchase(productSku: string, idempotencyKey: string): Promise<PurchaseOutcome> {
    return call(this.baseUrl, "/v1/purchase", {
      method: "POST",
      headers: await this.auth(),
      body: { player: this.player, product_sku: productSku, idempotency_key: idempotencyKey },
    });
  }

  async openPack(entitlementId: string): Promise<{ items: GrantedItem[]; seed: string }> {
    return call(this.baseUrl, "/v1/packs/open", {
      method: "POST",
      headers: await this.auth(),
      body: { player: this.player, entitlement_id: entitlementId },
    });
  }

  /** Balance + inventory in one read (the shop screen's call). */
  async wallet(): Promise<Wallet> {
    return call(this.baseUrl, `/v1/wallet/${encodeURIComponent(this.player)}`, {
      headers: await this.auth(),
    });
  }

  async inventory(): Promise<Inventory> {
    return call(this.baseUrl, `/v1/inventory/${encodeURIComponent(this.player)}`, {
      headers: await this.auth(),
    });
  }

  async purchases(): Promise<{ purchases: PurchaseRecord[] }> {
    return call(this.baseUrl, `/v1/purchases/${encodeURIComponent(this.player)}`, {
      headers: await this.auth(),
    });
  }
}

/** Game-backend client. Holds GAME_SERVER_SECRET: server side only. */
export class GameEconomyServer {
  constructor(
    private readonly baseUrl: string,
    private readonly serverSecret: string,
  ) {}

  private get headers(): Headers {
    return { "x-game-server-key": this.serverSecret };
  }

  /**
   * Open a Mint account for a new player. Call ONCE per player and persist the
   * returned `account_id`; it is the `player` for every other call.
   */
  createPlayer(ownerRef: string) {
    return call<{ account_id: string; owner_ref: string }>(this.baseUrl, "/v1/players", {
      method: "POST",
      headers: this.headers,
      body: { owner_ref: ownerRef },
    });
  }

  /** Issue a player token after YOUR backend has authenticated the player. */
  issueToken(player: string, ttlSeconds?: number) {
    return call<{ player: string; token: string; expires_at: number }>(this.baseUrl, "/v1/tokens", {
      method: "POST",
      headers: this.headers,
      body: { player, ttl_seconds: ttlSeconds },
    });
  }

  /**
   * Credit premium currency for a purchase your backend already verified with
   * Apple / Google. Idempotent per (platform, transactionId). The amount comes
   * from the server's GAME_IAP_PRODUCTS table, not from this call.
   */
  creditIap(p: { player: string; platform: string; transactionId: string; productId: string }) {
    return call<IapCredit>(this.baseUrl, "/v1/iap/credit", {
      method: "POST",
      headers: this.headers,
      body: {
        player: p.player,
        platform: p.platform,
        transaction_id: p.transactionId,
        product_id: p.productId,
      },
    });
  }

  /** The store refunded the purchase: take the credited currency back. */
  reverseIap(p: { player: string; platform: string; transactionId: string; productId: string }) {
    return call<IapCredit>(this.baseUrl, "/v1/iap/reverse", {
      method: "POST",
      headers: this.headers,
      body: {
        player: p.player,
        platform: p.platform,
        transaction_id: p.transactionId,
        product_id: p.productId,
      },
    });
  }

  advancePass(player: string, passSku: string, tier: number) {
    return call<{ pass_sku: string; tier: number }>(this.baseUrl, "/v1/pass/advance", {
      method: "POST",
      headers: this.headers,
      body: { player, pass_sku: passSku, tier },
    });
  }

  refund(player: string, receiptId: string, reason?: string) {
    return call<{ refunded: boolean; idempotent_replay: boolean; purchase: PurchaseRecord }>(
      this.baseUrl,
      "/v1/refunds",
      { method: "POST", headers: this.headers, body: { player, receipt_id: receiptId, reason } },
    );
  }
}
