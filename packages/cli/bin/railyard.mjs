#!/usr/bin/env node
import { main } from "../src/cli.mjs";

const code = await main(process.argv.slice(2), {
  env: process.env,
  cwd: process.cwd(),
  out: (s) => process.stdout.write(`${s}\n`),
  err: (s) => process.stderr.write(`${s}\n`),
  now: () => Date.now(),
  fetchImpl: fetch,
});
process.exit(code);
