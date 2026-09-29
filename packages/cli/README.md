# railyard

Deploy a Next.js, Vite, Astro or static site to your own Cloudflare account with
one command. Every git branch gets its own preview, every deploy is listed in
your Railyard console, and a bad deploy is one command away from the version
before it.

```sh
npx railyard login     # opens a code page; approve it in the browser
npx railyard deploy    # builds and deploys the folder you are in
```

## What it does

- Finds your framework: Next.js (built for Cloudflare Workers), Vite, Astro
  (static output) or a plain `index.html`. Anything else stops with one line
  that says what to change.
- Uses Cloudflare's own login on your machine. The first deploy opens
  Cloudflare in your browser; Railyard never sees that login and never asks
  for a key.
- Names the Worker after your app on `main`, and `<app>-<branch>` on any other
  branch, so each branch has its own URL.
- Records each deploy in the console: URL, commit, branch, framework, how long
  it took and whether it worked.
- `railyard rollback` puts the previous version of the same branch back live.

## Commands

| Command | What it does |
|---|---|
| `railyard login` | Signs this machine in to your Railyard org |
| `railyard deploy [--app NAME] [--branch B] [--dir PATH]` | Builds and deploys |
| `railyard rollback [--app NAME] [--branch B]` | Goes back one version |
| `railyard deploys [--app NAME]` | Lists recent deploys |

Your Railyard sign-in is saved in `~/.config/railyard/credentials.json`, readable
only by you. There is no option that takes a key or a token.

## Where it runs

Deploys go to your own Cloudflare account. Railyard hosts nothing; it keeps the
deploy history and the console.
