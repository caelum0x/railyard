/**
 * Identified people (fork addition, PY3.2): `identify` links a visitor's
 * session to the app's own user id and keeps one profile per (app, user) with
 * first and last seen and the latest traits. Delete-by-user removes every row
 * of that person in the org's apps.
 *
 * Every query names the org and joins through `website.team_id`; SQL uses the
 * upstream `{{name::type}}` placeholders (bound parameters, never interpolation).
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);
export const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;

/** Links the session to the user (upstream's session_link and session.distinct_id). */
export const LINK_SESSION_SQL = `
  with s as (
    update session set distinct_id = {{distinctId}}
    where session_id = {{sessionId::uuid}} and website_id = {{websiteId::uuid}}
    returning session_id
  )
  insert into session_link (website_id, session_id, distinct_id, created_at)
  values ({{websiteId::uuid}}, {{sessionId::uuid}}, {{distinctId}}, {{at::timestamptz}})
  on conflict do nothing`;

/** Creates or updates the profile; later traits win key by key. */
export const UPSERT_PROFILE_SQL = `
  insert into railyard_profile (website_id, distinct_id, first_seen, last_seen, traits)
  values ({{websiteId::uuid}}, {{distinctId}}, {{at::timestamptz}}, {{at::timestamptz}}, {{traits::jsonb}})
  on conflict (website_id, distinct_id) do update set
    first_seen = least(railyard_profile.first_seen, excluded.first_seen),
    last_seen = greatest(railyard_profile.last_seen, excluded.last_seen),
    traits = railyard_profile.traits || excluded.traits`;

/** Profiles of the org (optionally one app), most recently seen first. */
export const PROFILES_SQL = `
  select p.website_id::text as app_id, p.distinct_id as user_id, p.first_seen, p.last_seen, p.traits,
         (select count(*) from session_link l where l.website_id = p.website_id and l.distinct_id = p.distinct_id) as sessions
  from railyard_profile p
  join website w on w.website_id = p.website_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
    and ({{appId}}::uuid is null or p.website_id = {{appId}}::uuid)
    and ({{userId}}::text is null or p.distinct_id = {{userId}}::text)
  order by p.last_seen desc, p.distinct_id
  limit {{limit::int}} offset {{offset::int}}`;

/** A person's most recent events (their linked sessions), for the profile view. */
export const PROFILE_EVENTS_SQL = `
  select e.created_at, e.website_id::text as app_id, e.event_type, e.event_name, e.url_path
  from website_event e
  join website w on w.website_id = e.website_id
  join session_link l on l.website_id = e.website_id and l.session_id = e.session_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null and l.distinct_id = {{userId}}
    and ({{appId}}::uuid is null or e.website_id = {{appId}}::uuid)
  order by e.created_at desc
  limit 100`;

/**
 * Removes one person from the org's apps: their events and event data, session
 * data, revenue, replays and heatmap rows, session links, sessions and profile.
 * One statement, so it is all or nothing. Returns the number of rows removed.
 */
export const ERASE_USER_SQL = `
  with ws as (
    select website_id from website
    where team_id = {{orgId::uuid}} and ({{appId}}::uuid is null or website_id = {{appId}}::uuid)
  ),
  sess as (
    select l.website_id, l.session_id from session_link l join ws using (website_id) where l.distinct_id = {{userId}}
    union
    select s.website_id, s.session_id from session s join ws using (website_id) where s.distinct_id = {{userId}}
  ),
  evs as (select e.event_id from website_event e join sess using (website_id, session_id)),
  d_event_data as (delete from event_data d using evs where d.website_event_id = evs.event_id returning 1),
  d_events as (delete from website_event e using sess where e.website_id = sess.website_id and e.session_id = sess.session_id returning 1),
  d_session_data as (delete from session_data d using sess where d.website_id = sess.website_id and d.session_id = sess.session_id returning 1),
  d_revenue as (delete from revenue r using sess where r.website_id = sess.website_id and r.session_id = sess.session_id returning 1),
  d_replay as (delete from session_replay r using sess where r.website_id = sess.website_id and r.session_id = sess.session_id returning 1),
  d_heatmap as (delete from heatmap_event h using sess where h.website_id = sess.website_id and h.session_id = sess.session_id returning 1),
  d_links as (delete from session_link l using ws where l.website_id = ws.website_id and l.distinct_id = {{userId}} returning 1),
  d_sessions as (delete from session s using sess where s.website_id = sess.website_id and s.session_id = sess.session_id returning 1),
  d_profile as (delete from railyard_profile p using ws where p.website_id = ws.website_id and p.distinct_id = {{userId}} returning 1)
  select (select count(*) from d_event_data) + (select count(*) from d_events) + (select count(*) from d_session_data)
       + (select count(*) from d_revenue) + (select count(*) from d_replay) + (select count(*) from d_heatmap)
       + (select count(*) from d_links) + (select count(*) from d_sessions) + (select count(*) from d_profile) as rows_deleted`;

export const ERASURE_REQUEST_SQL = `
  insert into railyard_erasure (request_id, team_id, website_id, distinct_id, requested_at)
  values ({{requestId::uuid}}, {{orgId::uuid}}, {{appId}}::uuid, {{userId}}, {{at::timestamptz}})`;

export const ERASURE_DONE_SQL = `
  update railyard_erasure set done_at = {{at::timestamptz}}, rows_deleted = {{rows::int}}
  where request_id = {{requestId::uuid}} and done_at is null`;

export const ERASURE_PENDING_SQL = `
  select request_id::text, team_id::text as org_id, website_id::text as app_id, distinct_id as user_id, requested_at
  from railyard_erasure where done_at is null order by requested_at limit 100`;

export interface Query {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

export interface Profile {
  app_id: string;
  user_id: string;
  first_seen: string;
  last_seen: string;
  traits: Record<string, unknown>;
  sessions: number;
}

const iso = (v: unknown) => new Date(v as string).toISOString();
const traitsOf = (v: unknown): Record<string, unknown> => (typeof v === 'string' ? JSON.parse(v) : ((v as Record<string, unknown>) ?? {}));

/** The org's profiles; a string says what was wrong with the request. */
export async function listProfiles(
  db: Query,
  orgId: string,
  q: { app?: string | null; user?: string | null; limit?: number; offset?: number },
): Promise<{ profiles: Profile[] } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const appId = q.app ?? null;
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const userId = q.user ?? null;
  if (userId !== null && !USER_ID.test(userId)) return 'user is not a valid user id';
  const limit = q.limit ?? 100;
  const offset = q.offset ?? 0;
  if (!Number.isInteger(limit) || limit < 1 || limit > 1000) return 'limit must be 1 to 1000';
  if (!Number.isInteger(offset) || offset < 0) return 'offset must be 0 or more';
  const rows = await db.query<Record<string, unknown>>(PROFILES_SQL, { orgId, appId, userId, limit, offset });
  return {
    profiles: rows.map((r) => ({
      app_id: String(r.app_id),
      user_id: String(r.user_id),
      first_seen: iso(r.first_seen),
      last_seen: iso(r.last_seen),
      traits: traitsOf(r.traits),
      sessions: Number(r.sessions),
    })),
  };
}

export async function profileEvents(db: Query, orgId: string, userId: string, appId: string | null) {
  const rows = await db.query<Record<string, unknown>>(PROFILE_EVENTS_SQL, { orgId, userId, appId });
  return rows.map((r) => ({
    at: iso(r.created_at),
    app_id: String(r.app_id),
    type: Number(r.event_type) === 1 ? 'pageview' : 'event',
    name: (r.event_name as string | null) ?? null,
    path: (r.url_path as string | null) ?? null,
  }));
}

/**
 * Delete by user id. The request is recorded first, then done at once; if the
 * delete fails, the row stays pending and the hourly sweep retries it, so it
 * is done within 24 hours.
 */
export async function eraseUser(
  db: Query,
  orgId: string,
  userId: string,
  appId: string | null,
  now: Date,
  newId: () => string,
): Promise<{ request_id: string; status: 'done' | 'pending'; rows_deleted: number | null; due_by: string } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  if (!USER_ID.test(userId)) return 'user is not a valid user id';
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const requestId = newId();
  await db.query(ERASURE_REQUEST_SQL, { requestId, orgId, appId, userId, at: now.toISOString() });
  const due = new Date(now.getTime() + 24 * 3600 * 1000).toISOString();
  try {
    const [r] = await db.query<{ rows_deleted: number | string }>(ERASE_USER_SQL, { orgId, appId, userId });
    const rows = Number(r?.rows_deleted ?? 0);
    await db.query(ERASURE_DONE_SQL, { requestId, at: new Date().toISOString(), rows });
    return { request_id: requestId, status: 'done', rows_deleted: rows, due_by: due };
  } catch {
    return { request_id: requestId, status: 'pending', rows_deleted: null, due_by: due };
  }
}

/** Retries pending delete requests (hourly). Returns how many were finished. */
export async function sweepErasures(db: Query): Promise<{ done: number; failed: number }> {
  const pending = await db.query<{ request_id: string; org_id: string; app_id: string | null; user_id: string }>(ERASURE_PENDING_SQL, {});
  let done = 0;
  let failed = 0;
  for (const p of pending) {
    try {
      const [r] = await db.query<{ rows_deleted: number | string }>(ERASE_USER_SQL, { orgId: p.org_id, appId: p.app_id, userId: p.user_id });
      await db.query(ERASURE_DONE_SQL, { requestId: p.request_id, at: new Date().toISOString(), rows: Number(r?.rows_deleted ?? 0) });
      done += 1;
    } catch {
      failed += 1;
    }
  }
  return { done, failed };
}
