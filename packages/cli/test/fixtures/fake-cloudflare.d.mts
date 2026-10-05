// Types for the fake Cloudflare API (QY1.5 tests).
export interface FakeDomain {
  id: string;
  hostname: string;
  service: string;
  zone_id: string;
  zone_name: string;
}
export interface FakeCloudflare {
  base: string;
  api: string;
  account: string;
  calls: { method: string; path: string; query: string; auth: string | null; body: string }[];
  domains: Map<string, FakeDomain>;
  close(): Promise<void>;
}
export declare function startCloudflare(opts?: { zones?: string[]; account?: string; tokens?: string[] }): Promise<FakeCloudflare>;
