// Egress guard for customer-supplied URLs: one DNS answer per request, the
// connection pinned to that validated address (no DNS-rebinding window),
// redirects never followed, and every alternative spelling of an internal
// address (decimal, octal, hex, IPv6-mapped) refused. No database needed.
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { checkUrlShape, DestinationRefused, isPrivateAddress, resolvePinned } from "../../src/exports/net-guard.ts";
import { guardedFetch, makeGuardedFetch, pinnedLookup } from "../../src/exports/guarded-fetch.ts";
import { DeliveryError, sendWebhook } from "../../src/exports/runner.ts";

interface Hit {
  host: string | undefined;
  path: string | undefined;
  body: string;
}
interface Srv {
  port: number;
  hits: Hit[];
  server: Server;
}
const readBody = (req: IncomingMessage) =>
  new Promise<string>((resolve) => {
    const parts: Buffer[] = [];
    req.on("data", (d) => parts.push(d));
    req.on("end", () => resolve(Buffer.concat(parts).toString("utf8")));
  });

async function serve(handler: (req: IncomingMessage) => { status: number; headers?: Record<string, string> }): Promise<Srv> {
  const s: Srv = { port: 0, hits: [], server: createServer() };
  s.server.on("request", async (req, res) => {
    s.hits.push({ host: req.headers.host, path: req.url, body: await readBody(req) });
    const r = handler(req);
    res.writeHead(r.status, r.headers).end(r.status === 204 ? undefined : "ok");
  });
  await new Promise<void>((r) => s.server.listen(0, "127.0.0.1", () => r()));
  s.port = (s.server.address() as AddressInfo).port;
  return s;
}

let target: Srv;
let internal: Srv;
let bouncer: Srv;
before(async () => {
  process.env.PA_EXPORTS_ALLOW_PRIVATE = "0";
  target = await serve(() => ({ status: 200 }));
  internal = await serve(() => ({ status: 200 }));
  bouncer = await serve(() => ({ status: 302, headers: { location: `http://127.0.0.1:${internal.port}/latest/meta-data/` } }));
});
after(() => {
  for (const s of [target, internal, bouncer]) s.server.close();
});

// In these tests 127.0.0.1 stands in for "a public address" and 10.0.0.9 for
// "an internal one", so a real local server can play the public destination.
const testRule = (ip: string) => ip !== "127.0.0.1";

test("private, loopback, link-local, metadata and reserved addresses are refused, in every IPv4/IPv6 spelling", () => {
  const blocked = [
    "127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "100.100.100.200", "0.0.0.0", "255.255.255.255", "224.0.0.1", "198.18.0.1",
    "::", "::1", "fd00::1", "fd00:ec2::254", "fe80::1", "fe80::1%lo0", "ff02::1", "fec0::1",
    "::ffff:10.0.0.1", "::ffff:127.0.0.1", "::ffff:7f00:1", "::ffff:a9fe:a9fe", "[::ffff:169.254.169.254]", "0:0:0:0:0:ffff:7f00:0001",
    "::127.0.0.1", "::ffff:0:7f00:1", "64:ff9b::a00:1", "2002:7f00:1::1", "2002:a9fe:a9fe::", "2001::1", "2001:db8::1", "not-an-ip",
  ];
  for (const ip of blocked) assert.equal(isPrivateAddress(ip), true, ip);
  for (const ip of ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111", "::ffff:8.8.8.8", "2a00:1450:4001:80b::200e", "2002:808:808::1"]) assert.equal(isPrivateAddress(ip), false, ip);
});

test("decimal, octal, hex and IPv6-mapped forms of internal addresses are refused without any DNS lookup", async () => {
  const resolve = async () => assert.fail("an address literal must never be resolved");
  for (const raw of [
    "https://2130706433/x", // 127.0.0.1 as one decimal number
    "https://0177.0.0.1/x", // octal
    "https://0x7f.0.0.1/x", // hex
    "https://127.1/x", // short form
    "https://0251.0376.0251.0376/latest/meta-data/", // 169.254.169.254 in octal
    "https://2852039166/latest/meta-data/", // 169.254.169.254 in decimal
    "https://[::ffff:127.0.0.1]/x",
    "https://[::ffff:a9fe:a9fe]/x",
    "https://[0:0:0:0:0:ffff:0a00:0001]/x",
  ]) {
    const u = checkUrlShape(raw, "The URL");
    await assert.rejects(resolvePinned(u, { resolve }), /not a public address/, raw);
    await assert.rejects(makeGuardedFetch({ resolve })(raw, { method: "POST", body: "{}" }), DestinationRefused, raw);
  }
});

test("DNS rebinding: the host is resolved once and the connection goes to the validated address", async () => {
  let calls = 0;
  // First answer public, every later answer internal: the classic rebinding pattern.
  const resolve = async (host: string) => {
    assert.equal(host, "rebind.example");
    calls += 1;
    return calls === 1 ? ["127.0.0.1"] : ["10.0.0.9"];
  };
  const f = makeGuardedFetch({ resolve, isBlocked: testRule, allowPrivate: () => false });
  const before = target.hits.length;
  const res = await f(`http://rebind.example:${target.port}/hooks/a?x=1`, { method: "POST", body: '{"a":1}', headers: { "content-type": "application/json" } });
  assert.equal(res.status, 200);
  assert.equal(await res.text(), "ok");
  assert.equal(calls, 1, "exactly one DNS lookup per request: the socket never asks again");
  const hit = target.hits[before];
  assert.equal(hit.host, `rebind.example:${target.port}`, "the Host header keeps the name");
  assert.equal(hit.path, "/hooks/a?x=1");
  assert.equal(hit.body, '{"a":1}');
  // The next request resolves again, sees the internal answer, and sends nothing.
  await assert.rejects(f(`http://rebind.example:${target.port}/hooks/a`, { method: "POST", body: "{}" }), /not a public address/);
  assert.equal(calls, 2);
  assert.equal(target.hits.length, before + 1, "nothing reached the server after the answer turned internal");
});

test("the pinned socket lookup answers only the validated address, in both callback forms", () => {
  const lookup = pinnedLookup({ address: "203.0.113.7", family: 4, host: "rebind.example" }) as unknown as (h: string, o: unknown, cb: (...a: unknown[]) => void) => void;
  lookup("internal.example", { all: true }, (err, list) => {
    assert.equal(err, null);
    assert.deepEqual(list, [{ address: "203.0.113.7", family: 4 }]);
  });
  lookup("internal.example", { family: 0 }, (err, address, family) => {
    assert.equal(err, null);
    assert.equal(address, "203.0.113.7");
    assert.equal(family, 4);
  });
});

test("a mixed DNS answer (one public, one internal) is refused", async () => {
  const f = makeGuardedFetch({ resolve: async () => ["8.8.8.8", "10.0.0.9"], allowPrivate: () => false });
  await assert.rejects(f("https://mixed.example/x"), /not a public address/);
});

test("the default guard refuses a name that resolves to loopback, and nothing is sent", async () => {
  const before = target.hits.length;
  await assert.rejects(guardedFetch(`http://localhost:${target.port}/x`, { method: "POST", body: "{}" }), /not a public address/);
  assert.equal(target.hits.length, before);
});

test("redirects are never followed, even when the caller asks for it, so a 302 to an internal address goes nowhere", async () => {
  const f = makeGuardedFetch({ resolve: async () => ["127.0.0.1"], isBlocked: testRule, allowPrivate: () => false });
  const internalBefore = internal.hits.length;
  const res = await f(`http://bounce.example:${bouncer.port}/hook`, { method: "POST", body: "{}", redirect: "follow" });
  assert.equal(res.status, 302);
  assert.equal(res.headers.get("location"), `http://127.0.0.1:${internal.port}/latest/meta-data/`);
  assert.equal(internal.hits.length, internalBefore, "the redirect target was never contacted");

  // And the webhook sender reports the 3xx as a failed delivery.
  process.env.PA_EXPORTS_ALLOW_PRIVATE = "1"; // lets checkUrlShape accept http:// for the local server
  try {
    await assert.rejects(sendWebhook({ url: `http://bounce.example:${bouncer.port}/hook` }, "whsec_dGVzdA==", "{}", "msg_1", new Date(), f), (err: unknown) => err instanceof DeliveryError && err.httpStatus === 302);
  } finally {
    process.env.PA_EXPORTS_ALLOW_PRIVATE = "0";
  }
  assert.equal(internal.hits.length, internalBefore);
});

test("a refused destination surfaces as a delivery error with the guard's message", async () => {
  process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";
  try {
    const f = makeGuardedFetch({ resolve: async () => ["10.0.0.9"], allowPrivate: () => false });
    await assert.rejects(sendWebhook({ url: "http://internal.example/hook" }, "whsec_dGVzdA==", "{}", "msg_2", new Date(), f), (err: unknown) => err instanceof DeliveryError && /not a public address/.test(err.message));
  } finally {
    process.env.PA_EXPORTS_ALLOW_PRIVATE = "0";
  }
});

test("a signed Request object (the S3 path) is sent with its method, headers and body", async () => {
  const f = makeGuardedFetch({ resolve: async () => ["127.0.0.1"], isBlocked: testRule, allowPrivate: () => false });
  const before = target.hits.length;
  const req = new Request(`http://bucket.example:${target.port}/b/k.ndjson.gz`, { method: "PUT", headers: { "x-amz-content-sha256": "abc" }, body: new Uint8Array([1, 2, 3]) });
  const res = await f(req);
  assert.equal(res.status, 200);
  assert.equal(target.hits[before].path, "/b/k.ndjson.gz");
  assert.equal(target.hits[before].body.length, 3);
});

test("only http(s) URLs without credentials are sent", async () => {
  const f = makeGuardedFetch({ resolve: async () => ["8.8.8.8"] });
  await assert.rejects(f("ftp://files.example/x"), DestinationRefused);
  await assert.rejects(f("https://user:pw@files.example/x"), /credentials|user name/);
});
