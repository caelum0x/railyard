// Rithub self-serve provisioning (backlog item 4): one click from a signed-in
// Console session creates a dedicated Gitea user, a private org and a first
// repo -- instead of a separate native Gitea signup form. Idempotent at this
// route's level: once an org has gitea_repo_url set, calling again just
// returns the same result (never re-provisions, never leaks another org's
// account into a new one).

import { sessionFromRequest } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { json, sameOrigin } from "../../../../../lib/http.ts";

const FIRST_REPO_NAME = "first-repo";

export async function POST(req: Request) {
  const { cfg, repo, gitea } = deps();
  const s = await sessionFromRequest(req);
  if (!s) return json({ error: "Sign in again." }, 401);
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const org = await repo.getOrg(s.org);
  if (!org) return json({ error: "Organization not found." }, 404);
  if (org.gitea_repo_url) {
    return json({ username: org.gitea_username, org_slug: org.gitea_org_slug, repo_url: org.gitea_repo_url });
  }
  if (!gitea) return json({ error: "Rithub provisioning is not configured yet. Set GITEA_API_URL and GITEA_ADMIN_TOKEN." }, 503);
  try {
    const result = await gitea.provision({ seed: org.name || s.email, email: s.email, repoName: FIRST_REPO_NAME });
    const saved = await repo.setOrgGitea(org.id, { username: result.username, orgSlug: result.orgSlug, repoUrl: result.htmlUrl });
    return json(
      {
        username: saved?.gitea_username ?? result.username,
        org_slug: saved?.gitea_org_slug ?? result.orgSlug,
        repo_url: saved?.gitea_repo_url ?? result.htmlUrl,
      },
      201,
    );
  } catch (err) {
    console.error(`rithub provisioning failed for org ${org.id}: ${(err as Error).message}`);
    return json({ error: "Could not reach Rithub. Try again." }, 502);
  }
}
