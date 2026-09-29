// After Scribase Auth returns a user: find or create their org and its
// entitlements row, and seal the session.

import type { Repo } from "./repo.ts";
import { orgNameFor, type SignedInUser } from "./scribase-auth.ts";
import { sealSession } from "./session.ts";
import { emptyEntitlement } from "./types.ts";

export async function completeSignIn(repo: Repo, sessionSecret: string, user: SignedInUser, now: Date) {
  const org = (await repo.findOrgByUser(user.id)) ?? (await repo.createOrgForUser({ userId: user.id, email: user.email, name: orgNameFor(user.email) }));
  if (!(await repo.getEntitlement(org.id))) await repo.saveEntitlement(emptyEntitlement(org.id, now));
  const cookie = await sealSession(sessionSecret, { uid: user.id, email: user.email, org: org.id }, now);
  return { org, cookie };
}
