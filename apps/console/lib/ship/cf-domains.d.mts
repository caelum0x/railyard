// Types of cf-domains.mjs (a byte copy of packages/cli/src/domains.mjs, QY1.5).

export declare const CF_API: string;
export declare class DomainError extends Error {
  status: number;
  constructor(message: string, status: number);
}
export interface CfDomain {
  id: string;
  hostname: string;
  service: string;
  zone_id: string;
  zone_name: string;
}
export type DomainState = "active" | "pending" | "error";
export interface CloudflareDomains {
  zoneFor(hostname: string): Promise<{ id: string; name: string }>;
  list(service?: string): Promise<CfDomain[]>;
  attach(hostname: string, service: string, zoneId: string): Promise<CfDomain>;
  detach(id: string): Promise<void>;
  certStatus(zoneId: string, hostname: string): Promise<string | null>;
}
export declare function cleanHostname(raw: unknown): string | null;
export declare function zoneCandidates(hostname: string): string[];
export declare function domainState(certStatus: string | null | undefined): DomainState;
export declare function cloudflareDomains(opts: { token: string; accountId: string; fetchImpl?: typeof fetch; apiBase?: string }): CloudflareDomains;
export declare function addDomain(cf: CloudflareDomains, hostname: string, service: string): Promise<CfDomain & { state: DomainState; created: boolean }>;
export declare function removeDomain(cf: CloudflareDomains, hostname: string): Promise<{ hostname: string; removed: boolean }>;
export declare function listDomains(cf: CloudflareDomains, service?: string): Promise<(CfDomain & { state: DomainState })[]>;
