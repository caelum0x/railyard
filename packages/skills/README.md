# Railyard plugin for coding agents

One skill and one MCP server so a coding agent can deploy a project to the
user's own Cloudflare account, list and roll back deploys, and answer "how many
people used it" from real numbers.

## Install

Claude Code, from this folder:

```sh
claude plugin install ./packages/skills
```

Or add the MCP server alone:

```sh
claude mcp add railyard -- npx -y railyard-mcp
```

Then sign in once in a terminal:

```sh
npx railyard login
```

## What is inside

- `skills/railyard/SKILL.md`: when to deploy, how to handle the Cloudflare login in the
  browser, what to say when there is no data yet.
- `.claude-plugin/plugin.json`: the plugin manifest; it starts `railyard-mcp`.

The tools are listed in `packages/mcp/README.md`. All of them are read-only for
the agent; the deploy itself runs as a command on the user's machine.
