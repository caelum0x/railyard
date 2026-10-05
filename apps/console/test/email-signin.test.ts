// Email sign-in: Scribase Auth mails a one-time code; the console redeems it
// and opens the same session the OAuth path opens. No Google or GitHub app.

import { test } from "node:test";
import assert from "node:assert/strict";

import { startEmailSignIn, verifyEmailSignIn } from "../lib/email-signin.ts";
import { MemoryRepo } from "../lib/repo.ts";
import { EMAIL_COOKIE, normalizeCode, normalizeEmail, oauthProvidersFrom } from "../lib/scribase-auth.ts";
import { openSession, SESSION_COOKIE } from "../lib/session.ts";
import { SESSION_SECRET } from "./helpers.ts";

const BASE = "https://railyard.ereboslab.com";
const SCRIBASE = "https://ry.scribase.com";
const USER = { id: "7f1c9a70-0000-4000-8000-000000000002", email: "Founder@Example.com" };

function fakeAuth(opts: { otpStatus?: number; userEmail?: string } = {}) {
  const calls: { url: string; body: any }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    const body = init.body ? JSON.parse(String(init.body)) : null;
    calls.push({ url, body });
    assert.equal(new Headers(init.headers).get("apikey"), "anon");
    if (url === `${SCRIBASE}/auth/v1/otp`) return new Response("{}", { status: opts.otpStatus ?? 200 });
    if (url === `${SCRIBASE}/auth/v1/verify`) {
      if (body.type !== "email" || body.token !== "A1B2C3" || body.email !== "founder@example.com") return new Response("{}", { status: 403 });
      return new Response(JSON.stringify({ access_token: "at-email", user: { id: "spoofed" } }), { status: 200 });
    }
    if (url === `${SCRIBASE}/auth/v1/user`) {
      if (new Headers(init.headers).get("authorization") !== "Bearer at-email") return new Response("{}", { status: 401 });
      return new Response(JSON.stringify({ ...USER, email: opts.userEmail ?? USER.email }), { status: 200 });
    }
    return new Response("{}", { status: 404 });
  }) as typeof fetch;
  return { fetchImpl, calls };
}

function deps(fetchImpl: typeof fetch, repo = new MemoryRepo(() => new Date("2026-10-06T10:00:00Z"))) {
  return {
    cfg: { consoleUrl: BASE, scribaseUrl: SCRIBASE, scribaseAnonKey: "anon", sessionSecret: SESSION_SECRET },
    repo,
    fetchImpl,
    now: () => new Date("2026-10-06T10:00:00Z"),
  };
}

function post(path: string, fields: Record<string, string>, headers: Record<string, string> = { origin: BASE }) {
  const form = new FormData();
  for (const [k, v] of Object.entries(fields)) form.set(k, v);
  return new Request(`${BASE}${path}`, { method: "POST", body: form, headers });
}

function setCookies(res: Response): string[] {
  return res.headers.getSetCookie();
}

test("email code sign-in opens a session for the address the code was sent to", async () => {
  const auth = fakeAuth();
  const d = deps(auth.fetchImpl);
  const start = await startEmailSignIn(post("/auth/email", { email: "  Founder@Example.com " }), d);
  assert.equal(start.status, 303);
  assert.equal(start.headers.get("location"), `${BASE}/?step=code`);
  assert.deepEqual(auth.calls[0].body, { email: "founder@example.com", create_user: true });
  const emailCookie = setCookies(start).find((c) => c.startsWith(`${EMAIL_COOKIE}=`))!;
  assert.match(emailCookie, /HttpOnly; Secure; SameSite=Lax; Max-Age=900/);

  const pair = emailCookie.split(";")[0];
  const done = await verifyEmailSignIn(post("/auth/email/verify", { code: " A1B2 C3 " }, { origin: BASE, cookie: pair }), d);
  assert.equal(done.status, 303);
  assert.equal(done.headers.get("location"), `${BASE}/apps`);
  const session = setCookies(done).find((c) => c.startsWith(`${SESSION_COOKIE}=`))!.split(";")[0].split("=")[1];
  const opened = await openSession(SESSION_SECRET, session, new Date("2026-10-06T10:00:00Z"));
  assert.equal(opened?.email, "founder@example.com");
  assert.equal(opened?.uid, USER.id);
  assert.ok(setCookies(done).some((c) => c.startsWith(`${EMAIL_COOKIE}=;`)), "the address cookie is cleared");
  assert.ok(await d.repo.findOrgByUser(USER.id), "the org was created on first sign-in");
});

test("a wrong code goes back to the code step without a session", async () => {
  const d = deps(fakeAuth().fetchImpl);
  const res = await verifyEmailSignIn(post("/auth/email/verify", { code: "ZZZZZZ" }, { origin: BASE, cookie: `${EMAIL_COOKIE}=founder%40example.com` }), d);
  assert.equal(res.headers.get("location"), `${BASE}/?step=code&signin=code`);
  assert.ok(!setCookies(res).some((c) => c.startsWith(`${SESSION_COOKIE}=`)));
});

test("a user record for another address is refused", async () => {
  const d = deps(fakeAuth({ userEmail: "someone-else@example.com" }).fetchImpl);
  const res = await verifyEmailSignIn(post("/auth/email/verify", { code: "A1B2C3" }, { origin: BASE, cookie: `${EMAIL_COOKIE}=founder%40example.com` }), d);
  assert.equal(res.headers.get("location"), `${BASE}/?step=code&signin=code`);
});

test("cross-site posts are refused and nothing is sent", async () => {
  const auth = fakeAuth();
  const d = deps(auth.fetchImpl);
  assert.equal((await startEmailSignIn(post("/auth/email", { email: "a@example.com" }, { origin: "https://evil.example" }), d)).status, 403);
  assert.equal((await verifyEmailSignIn(post("/auth/email/verify", { code: "A1B2C3" }, { origin: "https://evil.example", cookie: `${EMAIL_COOKIE}=a%40example.com` }), d)).status, 403);
  assert.equal(auth.calls.length, 0);
});

test("a bad address never reaches the auth server; an expired step starts over", async () => {
  const auth = fakeAuth();
  const d = deps(auth.fetchImpl);
  const bad = await startEmailSignIn(post("/auth/email", { email: "not-an-address" }), d);
  assert.equal(bad.headers.get("location"), `${BASE}/?signin=email`);
  assert.equal(auth.calls.length, 0);
  const expired = await verifyEmailSignIn(post("/auth/email/verify", { code: "A1B2C3" }), d);
  assert.equal(expired.headers.get("location"), `${BASE}/?signin=expired`);
});

test("the auth server refusing to send shows a failed sign-in", async () => {
  const d = deps(fakeAuth({ otpStatus: 429 }).fetchImpl);
  const res = await startEmailSignIn(post("/auth/email", { email: "a@example.com" }), d);
  assert.equal(res.headers.get("location"), `${BASE}/?signin=failed`);
  assert.ok(!setCookies(res).some((c) => c.startsWith(`${EMAIL_COOKIE}=`)));
});

test("input normalisers and the OAuth button list", () => {
  assert.equal(normalizeEmail("A@B.co"), "a@b.co");
  for (const bad of ["", "a@b", "a b@c.de", "<a@b.co>", "a@-b.co", 42, null]) assert.equal(normalizeEmail(bad), null);
  assert.equal(normalizeCode(" 12 34 56 "), "123456");
  for (const bad of ["12", "12345678901234", "12-34", null]) assert.equal(normalizeCode(bad), null);
  assert.deepEqual(oauthProvidersFrom(undefined), ["google", "github"]);
  assert.deepEqual(oauthProvidersFrom(""), []);
  assert.deepEqual(oauthProvidersFrom("GitHub, other"), ["github"]);
});
