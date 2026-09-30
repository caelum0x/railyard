// The one workflow file ship on push adds to a repository (PY3.1). It holds no
// key and no secret reference: the job asks GitHub for an OIDC token (audience
// = the console) and `railyard ci` exchanges it for a short-lived Cloudflare
// token. Builds run on the customer's own GitHub Actions minutes.

export const WORKFLOW_PATH = ".github/workflows/railyard.yml";
export const CLI_VERSION = "0.1.0";

export function workflowYaml(opts: { consoleUrl: string; productionBranch: string; cliVersion?: string }): string {
  const console = opts.consoleUrl.replace(/\/$/, "");
  if (!/^https:\/\/[a-z0-9.-]+(:\d+)?$/i.test(console)) throw new Error("console URL must be https");
  const version = opts.cliVersion ?? CLI_VERSION;
  return `# Railyard: deploy to your own Cloudflare account on push.
# A push to ${opts.productionBranch} goes live; a push to any other branch deploys a preview
# and Railyard posts its URL on that branch's pull request.
# No key is stored in this repository. The job proves which repository it runs for
# with GitHub's OIDC token; Railyard answers with a short-lived Cloudflare token.
name: Railyard

on:
  push:
    branches: ["**"]
  workflow_dispatch: {}

permissions:
  contents: read
  id-token: write

concurrency:
  group: railyard-\${{ github.ref }}
  cancel-in-progress: true

jobs:
  deploy:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
      - name: Install dependencies
        run: |
          if [ -f package-lock.json ]; then npm ci
          elif [ -f pnpm-lock.yaml ]; then corepack enable && pnpm install --frozen-lockfile
          elif [ -f yarn.lock ]; then corepack enable && yarn install --frozen-lockfile
          elif [ -f package.json ]; then npm install
          fi
      - name: Deploy
        run: npx --yes railyard@${version} ci
        env:
          RAILYARD_CONSOLE_URL: ${console}
`;
}
