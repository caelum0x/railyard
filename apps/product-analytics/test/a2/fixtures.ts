// Shared generated fixtures for the insight tests and the capacity run.
// Deterministic (seeded): the same options always give the same events.
// Written straight into the tables the capture path fills (events with
// person_id on every row, persons, distinct ids, merge overrides, groups),
// so a million events load in seconds.

import pg from "pg";

export interface FixtureOptions {
  projectId: string;
  persons: number;
  days: number;
  /** Fixture "now": events happen in the `days` days before it. */
  end: Date;
  seed?: number;
  /** Chance a person comes back on a later day (default 0.3). */
  returnRate?: number;
}

export interface FixtureStats {
  events: number;
  persons: number;
  merged: number;
}

function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function uuidFrom(r: () => number): string {
  const h = Array.from({ length: 32 }, () => Math.floor(r() * 16).toString(16));
  h[12] = "4";
  h[16] = ((parseInt(h[16], 16) & 0x3) | 0x8).toString(16);
  const s = h.join("");
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}

export const URLS = ["https://shop.example/", "https://shop.example/pricing", "https://shop.example/docs", "https://shop.example/blog", "https://shop.example/signup"];
export const SCREENS = ["Home", "Search", "Cart", "Checkout", "Settings"];
export const COUNTRIES = ["US", "DE", "TR", "BR"];
export const PLANS = ["free", "pro", "team"];
export const BUY_CHAIN = 'button.btn.primary:text="Buy now"nth-child="1"nth-of-type="1"attr_id="buy"attr__class="btn primary"attr__id="buy";form:nth-child="2"nth-of-type="1";body:nth-child="1"nth-of-type="1"';
export const PRICING_CHAIN = 'a.nav-link:href="/pricing"text="Pricing"nth-child="3"nth-of-type="3"attr__href="/pricing"attr__class="nav-link";nav:nth-child="1"nth-of-type="1";body:nth-child="1"nth-of-type="1"';

interface Ev {
  uuid: string;
  event: string;
  distinct_id: string;
  person_id: string;
  ts: number;
  properties: Record<string, unknown>;
  elements_chain: string | null;
  session_id: string;
  group0: string | null;
}

const MIN = 60_000;
const DAY = 86_400_000;

/** Generates, inserts and returns counts. `client` is an owner connection (no RLS). */
export async function loadFixture(client: pg.Client | pg.PoolClient, o: FixtureOptions): Promise<FixtureStats> {
  const r = rng(o.seed ?? 42);
  const pick = <T>(xs: T[]) => xs[Math.floor(r() * xs.length)];
  const start = o.end.getTime() - o.days * DAY;
  const returnRate = o.returnRate ?? 0.3;
  let events = 0;
  let merged = 0;
  const months = new Set<string>();
  let buf: Ev[] = [];
  const persons: { id: string; created: number; props: Record<string, unknown> }[] = [];
  const ids: { d: string; p: string }[] = [];
  const overrides: { old: string; neu: string }[] = [];
  const companies = new Set<string>();

  const flush = async () => {
    if (!buf.length) return;
    const cols = {
      uuid: buf.map((e) => e.uuid),
      event: buf.map((e) => e.event),
      distinct_id: buf.map((e) => e.distinct_id),
      person_id: buf.map((e) => e.person_id),
      ts: buf.map((e) => new Date(e.ts).toISOString()),
      props: buf.map((e) => JSON.stringify(e.properties)),
      chain: buf.map((e) => e.elements_chain),
      session: buf.map((e) => e.session_id),
      g0: buf.map((e) => e.group0),
    };
    for (const e of buf) months.add(new Date(e.ts).toISOString().slice(0, 7));
    for (const m of months) await client.query("select pa.ensure_month($1::date)", [`${m}-01`]);
    await client.query(
      `insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp", properties, elements_chain, session_id, "$group_0")
       select u, $1, ev, d, p, t, pr::jsonb, ch, s, g
         from unnest($2::uuid[], $3::text[], $4::text[], $5::uuid[], $6::timestamptz[], $7::text[], $8::text[], $9::text[], $10::text[]) as x(u, ev, d, p, t, pr, ch, s, g)`,
      [o.projectId, cols.uuid, cols.event, cols.distinct_id, cols.person_id, cols.ts, cols.props, cols.chain, cols.session, cols.g0],
    );
    events += buf.length;
    buf = [];
  };

  for (let i = 0; i < o.persons; i += 1) {
    const personId = uuidFrom(r);
    const user = `user_${i}`;
    const mergedAnon = r() < 0.3;
    const anon = `anon_${i}`;
    const anonPerson = uuidFrom(r);
    const mobile = r() < 0.15;
    const company = `co_${i % 50}`;
    companies.add(company);
    const props = { email: `${user}@example.com`, country: pick(COUNTRIES), plan: pick(PLANS) };
    const firstDay = Math.floor(r() * o.days);
    let created = Number.POSITIVE_INFINITY;
    let signups = 0;
    for (let day = firstDay; day < o.days; day += 1) {
      if (day !== firstDay && r() >= returnRate) continue;
      const sessions = 1 + (r() < 0.3 ? 1 : 0);
      let t = start + day * DAY + Math.floor(r() * 10) * 3600_000 + Math.floor(r() * 60) * MIN;
      for (let s = 0; s < sessions; s += 1) {
        const sessionId = uuidFrom(r);
        const anonNow = mergedAnon && day === firstDay && s === 0;
        const did = anonNow ? anon : user;
        const pid = anonNow ? anonPerson : personId;
        const push = (event: string, properties: Record<string, unknown>, chain: string | null = null) => {
          if (t >= o.end.getTime()) return;
          created = Math.min(created, t);
          buf.push({ uuid: uuidFrom(r), event, distinct_id: did, person_id: pid, ts: t, properties, elements_chain: chain, session_id: sessionId, group0: r() < 0.9 ? company : null });
          t += (1 + Math.floor(r() * 4)) * MIN;
        };
        const views = 2 + Math.floor(r() * 5);
        let url = URLS[0];
        for (let v = 0; v < views; v += 1) {
          if (mobile) push("$screen", { $screen_name: pick(SCREENS) });
          else push("$pageview", { $current_url: url });
          url = r() < 0.4 ? URLS[Math.min(URLS.length - 1, URLS.indexOf(url) + 1)] : pick(URLS);
          if (r() < 0.2) push("$autocapture", { $event_type: "click", $current_url: url }, r() < 0.5 ? BUY_CHAIN : PRICING_CHAIN);
        }
        if ((day === firstDay && r() < 0.8) || r() < 0.08) {
          push("signup", { source: pick(["ads", "search", "friend", null]) });
          signups += 1;
          if (r() < 0.15) push("signup", { source: "retry" });
        }
        if (signups > 0 && r() < 0.35) push("activate", {});
        if (r() < 0.12) push("purchase", { amount: Math.round(r() * 20000) / 100, currency: "USD" });
        t += 45 * MIN; // the next session starts after 30 minutes of quiet
        if (buf.length >= 5000) await flush();
      }
    }
    if (created === Number.POSITIVE_INFINITY) continue;
    persons.push({ id: personId, created, props });
    ids.push({ d: user, p: personId });
    if (mergedAnon) {
      ids.push({ d: anon, p: personId });
      overrides.push({ old: anonPerson, neu: personId });
      merged += 1;
    }
  }
  await flush();
  for (let i = 0; i < persons.length; i += 5000) {
    const part = persons.slice(i, i + 5000);
    await client.query(
      `insert into pa.persons (id, project_id, properties, is_identified, created_at)
       select id, $1, pr::jsonb, true, c from unnest($2::uuid[], $3::text[], $4::timestamptz[]) as x(id, pr, c)`,
      [o.projectId, part.map((p) => p.id), part.map((p) => JSON.stringify(p.props)), part.map((p) => new Date(p.created).toISOString())],
    );
  }
  for (let i = 0; i < ids.length; i += 5000) {
    const part = ids.slice(i, i + 5000);
    await client.query(
      "insert into pa.person_distinct_ids (project_id, distinct_id, person_id) select $1, d, p from unnest($2::text[], $3::uuid[]) as x(d, p)",
      [o.projectId, part.map((x) => x.d), part.map((x) => x.p)],
    );
  }
  for (let i = 0; i < overrides.length; i += 5000) {
    const part = overrides.slice(i, i + 5000);
    await client.query(
      "insert into pa.person_overrides (project_id, old_person_id, new_person_id) select $1, o, n from unnest($2::uuid[], $3::uuid[]) as x(o, n)",
      [o.projectId, part.map((x) => x.old), part.map((x) => x.neu)],
    );
  }
  await client.query("insert into pa.group_types (project_id, group_type, group_type_index) values ($1, 'company', 0) on conflict do nothing", [o.projectId]);
  const cs = [...companies];
  await client.query(
    `insert into pa.groups (project_id, group_type_index, group_key, properties)
     select $1, 0, k, jsonb_build_object('industry', case when right(k, 1) in ('0','2','4','6','8') then 'retail' else 'software' end)
       from unnest($2::text[]) as k on conflict do nothing`,
    [o.projectId, cs],
  );
  await client.query("analyze pa.events");
  return { events, persons: persons.length, merged };
}

/** Inserts hand-written events for small exact cases. Person rows are created on first sight. */
export async function insertEvents(
  client: pg.Client | pg.PoolClient,
  projectId: string,
  rows: { event: string; person: string; at: string; properties?: Record<string, unknown>; distinct_id?: string; chain?: string; session?: string; created?: string }[],
): Promise<void> {
  const seen = new Set<string>();
  for (const e of rows) {
    await client.query("select pa.ensure_month($1::date)", [e.at.slice(0, 7) + "-01"]);
    if (!seen.has(e.person)) {
      seen.add(e.person);
      await client.query("insert into pa.persons (id, project_id, properties, is_identified, created_at) values ($1, $2, '{}', true, $3) on conflict (id) do nothing", [
        e.person,
        projectId,
        e.created ?? rows.filter((x) => x.person === e.person).map((x) => x.at).sort()[0],
      ]);
    }
    await client.query(
      `insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp", properties, elements_chain, session_id)
       values (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8)`,
      [projectId, e.event, e.distinct_id ?? e.person, e.person, e.at, JSON.stringify(e.properties ?? {}), e.chain ?? null, e.session ?? null],
    );
  }
}
