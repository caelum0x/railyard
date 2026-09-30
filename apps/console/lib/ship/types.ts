// Ship on push (Wave P PY3.1, RYP-E3, ADR RY-0004): records kept by the
// console (migration 0007). No row ever holds a plaintext credential: the
// Cloudflare grant is sealed (AES-256-GCM, lib/ai/seal.ts) under
// RAILYARD_CREDENTIALS_KEY with the org bound as associated data.

/** A GitHub App installation, owned by the org that linked a repo through it. */
export interface GithubInstallation {
  installation_id: number;
  org_id: string;
  account_login: string;
  created_at: string;
}

/** One repository that deploys one app on push. */
export interface ShipLink {
  id: string;
  org_id: string;
  app: string;
  repo_id: number;
  repo_full_name: string;
  installation_id: number;
  production_branch: string;
  workflow_pr_url: string | null;
  created_at: string;
}

/** The org's Cloudflare grant (self-managed OAuth client, authorization code with PKCE). */
export interface CloudflareConnection {
  org_id: string;
  account_id: string;
  account_name: string;
  refresh_nonce: string;
  refresh_ciphertext: string;
  /** A cached access token (sealed) so parallel jobs rarely refresh at once. */
  access_nonce: string | null;
  access_ciphertext: string | null;
  access_expires_at: string | null;
  updated_at: string;
}

export type LinkState = "started" | "github_done" | "linked" | "failed";

/** The browser half of `railyard link` (15 minutes, single use). */
export interface LinkSession {
  id_hash: string;
  org_id: string;
  app: string;
  repo_full_name: string;
  installation_id: number | null;
  pkce_verifier: string;
  state: LinkState;
  message: string | null;
  link_id: string | null;
  expires_at: string;
  created_at: string;
}
