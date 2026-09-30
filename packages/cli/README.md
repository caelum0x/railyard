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
| `railyard link [--app NAME]` | Deploys this GitHub repository on every push |
| `railyard ci` | The deploy step inside GitHub Actions (the workflow `link` adds runs it) |

Your Railyard sign-in is saved in `~/.config/railyard/credentials.json`, readable
only by you. There is no option that takes a key or a token.

## Ship from GitHub (arriving)

This is switched on per console with `RAILYARD_SHIP_ON_PUSH_LIVE`; until then
`railyard link` answers that it is not on yet.

```sh
npx railyard link
```

`link` prints one page to open. There you install the Railyard GitHub app on
the repository and allow Cloudflare access on Cloudflare's own screen; either
step is skipped when you did it before. Railyard then opens one pull request
that adds `.github/workflows/railyard.yml`. Once you merge it, each push to your
default branch goes live, and each other branch gets its own preview; Railyard
writes the preview URL on that branch's pull request.

The workflow holds no key. Each run proves which repository and branch it is
with GitHub's own identity token, and Railyard answers with a short-lived
Cloudflare token that is hidden in the log. Builds use your GitHub Actions
minutes and deploy to your Cloudflare account.

## In a Scribase project

Run `railyard deploy` in a folder where Scribase wrote the app's env file. The
project's URL and anon key (both public, they ship in the app) are set for the
build and on the Worker, so nothing is pasted and neither is printed; only
their names are shown. Anything secret in the same file, such as the service
role key, is left out and named. A git branch with its own Scribase branch
settings (`.scribase/branches/<branch>.env`) deploys its preview against that
branch.

## Where it runs

Deploys go to your own Cloudflare account. Railyard hosts nothing; it keeps the
deploy history and the console.
