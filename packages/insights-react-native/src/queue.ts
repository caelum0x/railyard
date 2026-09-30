// Copied from @scribase/expo's railyard/queue.ts (PY2.5, our own code) so this
// package has no dependency; keep the two in step.
/**
 * A small persistent send queue for the Railyard mobile clients (Wave P
 * PY2.5): events are saved first (AsyncStorage or anything with getItem /
 * setItem), sent in batches over plain HTTP, and removed only after the
 * server said yes. Each item carries an id the server de-duplicates on, so a
 * batch re-sent after a crash or a lost answer is stored once: an app that
 * was offline for ten minutes flushes every event exactly once.
 */

/** The slice of fetch these clients use (React Native, browsers, Node). */
export interface HttpResponse {
  status: number;
  ok: boolean;
  json(): Promise<unknown>;
}
export type HttpFetch = (url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }) => Promise<HttpResponse>;
type Later = (fn: () => void, ms: number) => unknown;

/** The platform's fetch and setTimeout, typed without the DOM library. */
export const platform = globalThis as unknown as { fetch?: HttpFetch; setTimeout?: Later };

export function defaultFetch(): HttpFetch {
  const f = platform.fetch;
  if (!f) throw new Error('no fetch on this platform; pass options.fetch');
  return (url, init) => f(url, init);
}

export function defaultLater(): Later {
  return (fn, ms) => platform.setTimeout?.(fn, ms);
}

export function hasOwn(object: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(object, key);
}

export interface KeyValueStorage {
  getItem(key: string): Promise<string | null> | string | null;
  setItem(key: string, value: string): Promise<void> | void;
}

export interface QueueItem<T> {
  id: string;
  at: number;
  body: T;
}

export interface QueueOptions<T> {
  storage?: KeyValueStorage | null;
  /** Storage key; one per client. */
  key: string;
  /** Sends one batch; resolves true when the server took all of it. */
  send: (batch: QueueItem<T>[]) => Promise<boolean>;
  maxBatch?: number;
  /** Oldest items are dropped past this many (a phone offline for days). */
  maxItems?: number;
  retryMs?: number;
  now?: () => number;
  setTimeout?: (fn: () => void, ms: number) => unknown;
  random?: () => string;
}

export interface Queue<T> {
  push(body: T): Promise<string>;
  flush(): Promise<number>;
  size(): number;
  ready(): Promise<void>;
}

function randomId(): string {
  const bytes = new Uint8Array(16);
  const c = (globalThis as { crypto?: { getRandomValues?: (a: Uint8Array) => Uint8Array } }).crypto;
  if (c?.getRandomValues) c.getRandomValues(bytes);
  else for (let i = 0; i < bytes.length; i += 1) bytes[i] = Math.floor(Math.random() * 256);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
}

export function createQueue<T>(options: QueueOptions<T>): Queue<T> {
  const maxBatch = options.maxBatch ?? 50;
  const maxItems = options.maxItems ?? 5_000;
  const retryMs = options.retryMs ?? 15_000;
  const now = options.now ?? Date.now;
  const later = options.setTimeout ?? defaultLater();
  const makeId = options.random ?? randomId;
  let items: QueueItem<T>[] = [];
  let sending = false;
  let rerun = false;
  let timer = false;
  let loaded: Promise<void> | null = null;

  const save = async () => {
    if (!options.storage) return;
    try {
      await options.storage.setItem(options.key, JSON.stringify(items));
    } catch {
      // Storage full: the items stay in memory for this session.
    }
  };

  const load = () => {
    loaded ??= (async () => {
      if (!options.storage) return;
      try {
        const raw = await options.storage.getItem(options.key);
        const saved = raw ? (JSON.parse(raw) as QueueItem<T>[]) : [];
        if (Array.isArray(saved)) items = [...saved.filter((i) => i && typeof i.id === 'string'), ...items];
      } catch {
        // A broken entry is dropped.
      }
    })();
    return loaded;
  };

  const schedule = () => {
    if (timer) return;
    timer = true;
    later(() => {
      timer = false;
      void flush();
    }, retryMs);
  };

  async function flush(): Promise<number> {
    await load();
    if (sending) {
      // Asked again while a batch is in flight: run once more right after it.
      rerun = true;
      return 0;
    }
    sending = true;
    rerun = false;
    let sent = 0;
    let failed = false;
    try {
      while (items.length) {
        const batch = items.slice(0, maxBatch);
        let ok = false;
        try {
          ok = await options.send(batch);
        } catch {
          ok = false;
        }
        if (!ok) {
          failed = true;
          schedule();
          break;
        }
        const done = new Set(batch.map((b) => b.id));
        items = items.filter((i) => !done.has(i.id));
        sent += batch.length;
        await save();
      }
    } finally {
      sending = false;
    }
    if (rerun && !failed) sent += await flush();
    return sent;
  }

  return {
    async push(body: T) {
      await load();
      const item = { id: makeId(), at: now(), body };
      items = [...items, item].slice(-maxItems);
      await save();
      void flush();
      return item.id;
    },
    flush,
    size: () => items.length,
    ready: load,
  };
}
