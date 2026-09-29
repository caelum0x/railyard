#!/usr/bin/env node
import { main } from "../src/cli.mjs";

const code = await main(process.argv.slice(2), {
  env: process.env,
  cwd: process.cwd(),
  out: (s) => process.stdout.write(`${s}\n`),
  err: (s) => process.stderr.write(`${s}\n`),
  now: () => Date.now(),
  fetchImpl: fetch,
  readStdin: async () => {
    let text = "";
    for await (const chunk of process.stdin) text += chunk;
    return text;
  },
});
process.exit(code);
