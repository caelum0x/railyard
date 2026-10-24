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
if (cmd === "tail") {
  // Stands in for `wrangler tail`'s real behaviour: it streams until something
  // kills it, it never exits on its own. Used to load-test `railyard logs`
  // against a long-running stream (many lines, one carrying a secret to prove
  // redaction still runs per-chunk) and against a Ctrl+C-style signal stop
  // (FAKE_WRANGLER_TAIL_SELF_SIGTERM simulates the terminal sending SIGTERM to
  // this child, the same way Ctrl+C would, without a second process to send it).
  const n = Number(process.env.FAKE_WRANGLER_TAIL_LINES ?? "0");
  const secret = process.env.FAKE_WRANGLER_TAIL_SECRET ?? "";
  for (let i = 0; i < n; i += 1) {
    // The secret rides on the very first line (whole in the first small write,
    // nowhere near a pipe chunk boundary) so the redaction assertion below can't
    // flake on the token happening to straddle two separate `data` events.
    console.log(JSON.stringify({ line: i, message: i === 0 && secret ? `token ${secret} seen` : `log line ${i}` }));
  }
  if (process.env.FAKE_WRANGLER_TAIL_SELF_SIGTERM === "1") process.kill(process.pid, "SIGTERM");
  else if (n > 0) process.exit(0);
  else setInterval(() => {}, 1000); // hang like a real tail until the parent kills us
  process.exitCode = 0;
} else {
  console.error(`fake wrangler: unknown command ${cmd}`);
  process.exit(1);
}
