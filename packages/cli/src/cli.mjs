// Argument parsing and dispatch. There is deliberately no flag that takes a
// key or token: sign-in is the device flow, Cloudflare is wrangler's login.

import { deploy, listDeploys, login, rollback } from "./commands.mjs";
import { linkScribase } from "./scribase/link.mjs";
import { link } from "./link/link.mjs";
import { ci } from "./link/ci.mjs";

const HELP = `railyard: ship it from your coding agent

  railyard login                      sign in (opens a code page in your browser)
  railyard deploy [--app NAME] [--branch B] [--dir PATH]
                                      build and deploy to your own Cloudflare account
  railyard rollback [--app NAME] [--branch B]
                                      go back to the previous version
  railyard deploys [--app NAME]       list recent deploys
  railyard link [--app NAME]          deploy this GitHub repository on every push
                                      (a preview URL on each branch's pull request)
  railyard ci                         the deploy step inside GitHub Actions (added by link)

  In a Scribase project folder, deploy uses the project's URL and anon key
  from its env file; secrets in that file are left out.`;

const FLAGS = new Set(["--app", "--branch", "--dir"]);

export function parse(argv) {
  const [command, ...rest] = argv;
  const opts = {};
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (!FLAGS.has(a)) return { error: `Unknown option ${a}. Run railyard --help.` };
    const v = rest[i + 1];
    if (!v || v.startsWith("--")) return { error: `${a} needs a value.` };
    opts[a.slice(2)] = v;
    i += 1;
  }
  return { command, opts };
}

export async function main(argv, ctx) {
  if (!argv.length || argv[0] === "--help" || argv[0] === "-h" || argv[0] === "help") {
    ctx.out(HELP);
    return 0;
  }
  const p = parse(argv);
  if (p.error) {
    ctx.err(p.error);
    return 2;
  }
  const commands = { login: () => login(ctx), deploy: () => deploy(ctx, p.opts), rollback: () => rollback(ctx, p.opts), deploys: () => listDeploys(ctx, p.opts),
    link: () => link(ctx, p.opts),
    ci: () => ci(ctx, p.opts),
    // Run by the Scribase CLI (the deploy slot), which pipes a one-time claim on stdin.
    "link-scribase": async () => (Object.keys(p.opts).length ? 2 : linkScribase(ctx, ctx.readStdin ? await ctx.readStdin() : "")) };
  const fn = commands[p.command];
  if (!fn) {
    ctx.err(`Unknown command ${p.command}. Run railyard --help.`);
    return 2;
  }
  try {
    return await fn();
  } catch (e) {
    ctx.err(e.status === 401 ? "Your sign-in expired. Run railyard login." : `Railyard could not finish: ${e.message}`);
    return 1;
  }
}
