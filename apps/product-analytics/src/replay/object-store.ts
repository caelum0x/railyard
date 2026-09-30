// Where recordings (and S3 exports) are written: an S3-compatible bucket
// (Scribase Storage's S3 API, R2, or any S3 endpoint) through the aws4fetch
// signer (MIT, vendored), a directory for local runs, or memory for tests.
// Keys are plain relative paths; nothing outside the store's root is reachable.

import { createHash } from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve, sep } from "node:path";
import { AwsClient } from "./vendor/aws4fetch/aws4fetch.esm.mjs";

export interface ObjectStore {
  readonly kind: "s3" | "fs" | "memory";
  put(key: string, body: Uint8Array, contentType: string): Promise<void>;
  get(key: string): Promise<Uint8Array | null>;
  delete(key: string): Promise<void>;
}

export const OBJECT_KEY = /^(?!.*\.\.)(?!\/)[A-Za-z0-9/_.=-]{1,1024}$/;

export class StoreError extends Error {
  status: number | null;
  constructor(message: string, status: number | null = null) {
    super(message);
    this.status = status;
  }
}

function checkKey(key: string): string {
  if (!OBJECT_KEY.test(key)) throw new StoreError("invalid object key");
  return key;
}

export class MemoryObjectStore implements ObjectStore {
  readonly kind = "memory" as const;
  objects = new Map<string, { body: Uint8Array; contentType: string }>();
  async put(key: string, body: Uint8Array, contentType: string) {
    this.objects.set(checkKey(key), { body: Uint8Array.from(body), contentType });
  }
  async get(key: string) {
    return this.objects.get(checkKey(key))?.body ?? null;
  }
  async delete(key: string) {
    this.objects.delete(checkKey(key));
  }
}

export class FsObjectStore implements ObjectStore {
  readonly kind = "fs" as const;
  root: string;
  constructor(root: string) {
    this.root = resolve(root);
  }
  path(key: string): string {
    const p = resolve(join(this.root, checkKey(key)));
    if (!p.startsWith(this.root + sep)) throw new StoreError("invalid object key");
    return p;
  }
  async put(key: string, body: Uint8Array) {
    const p = this.path(key);
    await mkdir(dirname(p), { recursive: true });
    await writeFile(p, body);
  }
  async get(key: string) {
    try {
      return new Uint8Array(await readFile(this.path(key)));
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw err;
    }
  }
  async delete(key: string) {
    await rm(this.path(key), { force: true });
  }
}

export interface S3Config {
  /** e.g. https://<account>.r2.cloudflarestorage.com or the Scribase Storage S3 URL. */
  endpoint: string;
  bucket: string;
  region: string;
  accessKeyId: string;
  secretAccessKey: string;
  /** Keys are written under this prefix (no leading or trailing slash). */
  prefix?: string;
}

export const BUCKET = /^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$/;

/** Path-style requests (endpoint/bucket/key), which every S3-compatible store accepts. */
export class S3ObjectStore implements ObjectStore {
  readonly kind = "s3" as const;
  cfg: S3Config;
  client: AwsClient;
  fetchImpl: typeof fetch;

  constructor(cfg: S3Config, fetchImpl: typeof fetch = fetch) {
    if (!BUCKET.test(cfg.bucket)) throw new StoreError("The bucket name is not valid.");
    let u: URL;
    try {
      u = new URL(cfg.endpoint);
    } catch {
      throw new StoreError("The endpoint must be a URL.");
    }
    if (u.protocol !== "https:" && u.protocol !== "http:") throw new StoreError("The endpoint must be an http or https URL.");
    this.cfg = { ...cfg, endpoint: cfg.endpoint.replace(/\/+$/, ""), prefix: (cfg.prefix ?? "").replace(/^\/+|\/+$/g, "") };
    // No retries inside the signer: callers own their retry and backoff.
    this.client = new AwsClient({ accessKeyId: cfg.accessKeyId, secretAccessKey: cfg.secretAccessKey, service: "s3", region: cfg.region || "auto", retries: 0 });
    this.fetchImpl = fetchImpl;
  }

  url(key: string): string {
    const full = this.cfg.prefix ? `${this.cfg.prefix}/${checkKey(key)}` : checkKey(key);
    return `${this.cfg.endpoint}/${this.cfg.bucket}/${full.split("/").map(encodeURIComponent).join("/")}`;
  }

  async send(method: string, key: string, body?: Uint8Array, contentType?: string): Promise<Response> {
    const headers: Record<string, string> = { "x-amz-content-sha256": createHash("sha256").update(body ?? new Uint8Array()).digest("hex") };
    if (contentType) headers["content-type"] = contentType;
    const signed = await this.client.sign(this.url(key), { method, headers, body: body ?? null });
    return this.fetchImpl(signed);
  }

  async put(key: string, body: Uint8Array, contentType: string) {
    const res = await this.send("PUT", key, body, contentType);
    if (!res.ok) throw new StoreError(`object store answered ${res.status} to a write`, res.status);
    await res.arrayBuffer();
  }

  async get(key: string) {
    const res = await this.send("GET", key);
    if (res.status === 404) return (await res.arrayBuffer(), null);
    if (!res.ok) throw new StoreError(`object store answered ${res.status} to a read`, res.status);
    return new Uint8Array(await res.arrayBuffer());
  }

  async delete(key: string) {
    const res = await this.send("DELETE", key);
    if (!res.ok && res.status !== 404) throw new StoreError(`object store answered ${res.status} to a delete`, res.status);
    await res.arrayBuffer();
  }
}

type Env = Record<string, string | undefined>;

/**
 * The recordings store from the environment:
 *   PA_OBJECT_STORE=s3  PA_S3_ENDPOINT, PA_S3_BUCKET, PA_S3_REGION (default auto),
 *                       PA_S3_ACCESS_KEY_ID, PA_S3_SECRET_ACCESS_KEY, PA_S3_PREFIX (optional)
 *   PA_OBJECT_STORE=fs  PA_FS_STORE_DIR
 * Unset: no store, so recording stays off in every app's config.
 */
export function objectStoreFromEnv(env: Env = process.env): ObjectStore | null {
  const kind = env.PA_OBJECT_STORE?.trim();
  if (!kind) return null;
  if (kind === "fs") {
    const dir = env.PA_FS_STORE_DIR?.trim();
    if (!dir) throw new Error("PA_FS_STORE_DIR is required when PA_OBJECT_STORE=fs");
    return new FsObjectStore(dir);
  }
  if (kind === "s3") {
    const need = (n: string) => {
      const v = env[n]?.trim();
      if (!v) throw new Error(`${n} is required when PA_OBJECT_STORE=s3`);
      return v;
    };
    return new S3ObjectStore({
      endpoint: need("PA_S3_ENDPOINT"),
      bucket: need("PA_S3_BUCKET"),
      region: env.PA_S3_REGION?.trim() || "auto",
      accessKeyId: need("PA_S3_ACCESS_KEY_ID"),
      secretAccessKey: need("PA_S3_SECRET_ACCESS_KEY"),
      prefix: env.PA_S3_PREFIX?.trim() || "",
    });
  }
  throw new Error("PA_OBJECT_STORE must be s3 or fs");
}

let configured: ObjectStore | null | undefined;

/** The process's recordings store (from the environment on first use). */
export function replayStore(): ObjectStore | null {
  if (configured === undefined) configured = objectStoreFromEnv();
  return configured;
}

/** Tests and tools set the store directly. */
export function useReplayStore(store: ObjectStore | null): void {
  configured = store;
}
