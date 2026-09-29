#!/usr/bin/env node
// Stand-in for the OpenNext Cloudflare build: writes .open-next/.
import { mkdirSync, writeFileSync, appendFileSync } from "node:fs";
if (process.env.FAKE_WRANGLER_LOG) appendFileSync(process.env.FAKE_WRANGLER_LOG, `${JSON.stringify({ cmd: "opennext", rest: process.argv.slice(2) })}\n`);
mkdirSync(".open-next/assets", { recursive: true });
writeFileSync(".open-next/worker.js", "export default { fetch() { return new Response('ok') } }");
console.log("OpenNext build complete.");
