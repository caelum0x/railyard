# railyard-mcp

The Railyard MCP server: ask your coding agent to deploy, list deploys, check
your plan and usage, and read visitor numbers, without leaving the chat.

## Set up

```sh
npx railyard login                       # once, in a terminal
claude mcp add railyard -- npx -y railyard-mcp
```

The server reads the sign-in that `railyard login` saved on this machine. It
never asks for a key and never prints one.

## Tools

| Tool | What it does | Changes anything |
|---|---|---|
| `deploy.run` | Returns the exact command to deploy this project; your agent runs it in the project folder | No |
| `deploy.list` | Recent deploys: URL, branch, commit, framework, duration, status | No |
| `insights.summary` | Visitors, page views and top pages for an app; says "no data yet" until events arrive | No |
| `insights.funnel` | Step-by-step conversion between pages or events; says "no data yet" until events arrive | No |
| `flags.list` | The org's flags per app and environment: key, on or off, value, description | No |
| `gateway.traces` | Recent AI gateway calls for the gateway key in this environment | No |
| `usage.current` | Plan state, trial days left, apps used against the plan | No |

Every tool is marked read-only for your agent. Deploys run on your own machine
through `npx railyard deploy`, into your own Cloudflare account.

## Environment

| Name | What |
|---|---|
| `RAILYARD_INSIGHTS_QUERY_URL` | Where Insights answers queries, once it is live |
| `RAILYARD_GATEWAY_URL`, `RAILYARD_GATEWAY_KEY` | Your gateway, for `gateway.traces` (the key your app already uses) |
