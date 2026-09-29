---
name: railyard
description: Deploy the current web project to the user's own Cloudflare account with Railyard, list deploys, roll back, and read visitor numbers. Use when the user asks to deploy, ship, publish or preview a Next.js, Vite, Astro or static site, to roll back a deploy, or to see how many people used their app.
---

# Railyard

Railyard deploys Next.js, Vite, Astro and static sites into the user's own
Cloudflare account and keeps a deploy history. Deploys run on the user's
machine.

## Deploy

1. If `usage.current` says the user is not signed in, run `npx railyard login`
   and ask the user to approve the code in the browser page it prints.
2. Call `deploy.run` (with `app` if the folder name is not the app name) and run
   the command it returns in the project folder.
3. The first deploy opens a Cloudflare login in the browser. Tell the user to
   finish it there. Never ask for a Cloudflare key.
4. Report the `Live:` URL from the output. On a branch other than main the URL
   is a preview: `<app>-<branch>`.

If the command stops with "Railyard deploys Next.js, Vite, Astro or static
sites", tell the user exactly what that line says; do not work around it.

## After a deploy

- `deploy.list` shows the history. `npx railyard rollback` puts the previous
  version of the same branch back live.
- `insights.summary` answers "how many people visited". If it returns
  `no_data_yet`, say so plainly; do not estimate.
- `usage.current` answers plan, trial and app-count questions.

## Rules

- Never invent numbers. Only report what a tool returned.
- Never ask the user to paste a key or token.
