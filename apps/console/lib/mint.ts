// Client for Mint (apps/mint)'s ledger API, used to provision a per-app
// billing account from the Apps tab (backlog item 11: "no UI, out-of-band"
// gap) instead of an operator curling `POST /v1/accounts` with the master
// key by hand. Server-only: the API key never reaches a client component.

export interface MintAccount {
  accountId: string;
}

export interface MintBalance {
  account_id: string;
  available_micros: number;
  held_micros: number;
}

export interface MintClient {
  /** `POST /v1/accounts` with kind "org", owner_ref = this app's id. */
  createAccount(ownerRef: string): Promise<MintAccount>;
  /** `GET /v1/accounts/:id/balance`. */
  balance(accountId: string): Promise<MintBalance>;
}

class HttpMintClient implements MintClient {
  constructor(private base: string, private apiKey: string, private fetchImpl: typeof fetch = fetch) {}

  private async req(path: string, init?: RequestInit): Promise<Response> {
    return this.fetchImpl(`${this.base}${path}`, {
      ...init,
      headers: { "content-type": "application/json", "x-api-key": this.apiKey, ...(init?.headers ?? {}) },
    });
  }

  async createAccount(ownerRef: string): Promise<MintAccount> {
    const res = await this.req("/v1/accounts", { method: "POST", body: JSON.stringify({ kind: "org", owner_ref: ownerRef }) });
    if (!res.ok) throw new Error(`mint account create failed with ${res.status}`);
    const body = (await res.json()) as { account_id?: string };
    if (!body.account_id) throw new Error("mint returned no account_id");
    return { accountId: body.account_id };
  }

  async balance(accountId: string): Promise<MintBalance> {
    const res = await this.req(`/v1/accounts/${encodeURIComponent(accountId)}/balance`);
    if (!res.ok) throw new Error(`mint balance lookup failed with ${res.status}`);
    return (await res.json()) as MintBalance;
  }
}

export function mintClient(opts: { url: string; apiKey: string; fetchImpl?: typeof fetch }): MintClient {
  return new HttpMintClient(opts.url.replace(/\/$/, ""), opts.apiKey, opts.fetchImpl);
}
