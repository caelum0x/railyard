// Insights depth for customers (PY3.2): profiles, cohorts, retention, breakdowns, export and delete by user.
import { deps } from "../../../../../lib/deps.ts";
import { forwardInsights } from "../../../../../lib/insights-depth/forward.ts";

export const dynamic = "force-dynamic";

async function handle(req: Request, { params }: { params: { path: string[] } }) {
  const { repo, cfg } = deps();
  return forwardInsights({ repo, insightsApi: cfg.insightsApi, live: cfg.replayLive }, req, params.path, new Date());
}

export const GET = handle;
export const POST = handle;
export const DELETE = handle;
