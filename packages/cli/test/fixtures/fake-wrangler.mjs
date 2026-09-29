#!/usr/bin/env node
// A stand-in for wrangler in tests: records its arguments and answers like
// wrangler 4 does for whoami, login, deploy and rollback.
import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";

const [cmd, ...rest] = process.argv.slice(2);
const log = process.env.FAKE_WRANGLER_LOG;
if (log) appendFileSync(log, `${JSON.stringify({ cmd, rest, cwd: process.cwd() })}\n`);
const state = process.env.FAKE_WRANGLER_STATE;
const loggedIn = () => state && existsSync(state) && JSON.parse(readFileSync(state, "utf8")).loggedIn;

if (cmd === "whoami") {
  if (loggedIn() || process.env.FAKE_WRANGLER_LOGGED_IN === "1") {
    console.log("You are logged in with an OAuth Token, associated with the email founder@example.com.");
    process.exit(0);
  }
  console.log("You are not authenticated. Please run `wrangler login`.");
  process.exit(1);
}
if (cmd === "login") {
  writeFileSync(state, JSON.stringify({ loggedIn: true }));
  console.log("Successfully logged in.");
  process.exit(0);
}
if (cmd === "deploy") {
  if (process.env.FAKE_WRANGLER_FAIL === "1") {
    console.error("X [ERROR] A request to the Cloudflare API failed.");
    process.exit(1);
  }
  const name = rest[rest.indexOf("--name") + 1];
  const n = Number(process.env.FAKE_WRANGLER_N ?? "1");
  console.log(`Uploaded ${name} (1.02 sec)`);
  console.log(`Deployed ${name} triggers (0.31 sec)`);
  console.log(`  https://${name}.founder.workers.dev`);
  console.log(`Current Version ID: 0000000${n}-aaaa-4bbb-8ccc-dddddddddddd`);
  process.exit(0);
}
if (cmd === "rollback") {
  console.log(`Successfully rolled back to Worker Version: ${rest[0]}`);
  console.log(`\nCurrent Version ID: ${rest[0]}`);
  process.exit(0);
}
console.error(`fake wrangler: unknown command ${cmd}`);
process.exit(1);
