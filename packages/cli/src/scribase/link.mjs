// `railyard link-scribase` (Wave P PY1.2): the Scribase CLI pipes a one-time
// claim from the Scribase bridge on stdin, as JSON {claim, console}. It is
// exchanged once at the console's device-token endpoint and the org token is
// saved to the credentials file (0600). No flag takes the claim or the token,
// and neither is ever printed.

import { consoleApi } from "../console-api.mjs";
import { saveCredentials } from "../credentials.mjs";

const CLAIM = /^[A-Za-z0-9_-]{20,200}$/;

export async function linkScribase(ctx, stdinText) {
  let input;
  try {
    input = JSON.parse(stdinText);
  } catch {
    ctx.err("link-scribase reads its input from the Scribase CLI; start the deploy step from Scribase instead.");
    return 2;
  }
  const base = typeof input?.console === "string" && /^https?:\/\//.test(input.console) ? input.console : ctx.env.RAILYARD_CONSOLE_URL;
  if (!CLAIM.test(String(input?.claim ?? "")) || !base) {
    ctx.err("The link from Scribase was not valid. Start the deploy step from Scribase again.");
    return 2;
  }
  const api = consoleApi({ console: base, fetchImpl: ctx.fetchImpl });
  const r = await api.deviceToken(input.claim);
  if (r.status !== 200 || !r.body.access_token) {
    ctx.err("The link from Scribase expired or was used already. Start the deploy step from Scribase again.");
    return 1;
  }
  const path = saveCredentials({ console: base, token: r.body.access_token, org: r.body.org_id, source: "scribase" }, ctx.env);
  ctx.out(`Linked to your Scribase organization. Saved to ${path} (only you can read it).`);
  return 0;
}
