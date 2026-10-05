// One mail to each recipient of an alert or a subscription, with the
// unsubscribe link every mail carries, and a log row per recipient
// (sent, failed, unsubscribed, not_configured).

import { createHmac, timingSafeEqual } from "node:crypto";
import type { Db } from "../db.ts";
import { escapeHtml, type Mailer } from "./mail.ts";

export interface Outgoing {
  projectId: string;
  source: "alert" | "subscription";
  sourceId: string;
  recipients: string[];
  subject: string;
  /** The body without the footer; the footer with the unsubscribe link is added per recipient. */
  html: string;
  text: string;
  /** Same key for a retried send of the same mail (with the recipient appended). */
  idempotencyKey: string;
}

export interface SendDeps {
  db: Db;
  mailer: Mailer;
  /** Signs unsubscribe links; without it no mail is sent (every mail must carry one). */
  secret: string | null;
  /** Public base of this service, for the unsubscribe link. */
  publicUrl: string;
}

export function unsubscribeToken(secret: string, projectId: string, email: string): string {
  return createHmac("sha256", secret).update(`unsubscribe:v1:${projectId}:${email.toLowerCase()}`).digest("base64url");
}

export function checkUnsubscribeToken(secret: string | null, projectId: string, email: string, token: string): boolean {
  if (!secret) return false;
  const want = Buffer.from(unsubscribeToken(secret, projectId, email));
  const got = Buffer.from(token);
  return want.length === got.length && timingSafeEqual(want, got);
}

export function unsubscribeUrl(publicUrl: string, secret: string, projectId: string, email: string): string {
  const q = new URLSearchParams({ p: projectId, e: email.toLowerCase(), t: unsubscribeToken(secret, projectId, email) });
  return `${publicUrl.replace(/\/+$/, "")}/v1/unsubscribe?${q}`;
}

export function footer(url: string): { html: string; text: string } {
  return {
    html: `<p style="color:#666;font-size:12px;margin-top:24px">You get this because someone on your team added your address to these alerts and reports. <a href="${escapeHtml(url)}">Unsubscribe from these emails</a>.</p>`,
    text: `\n\nYou get this because someone on your team added your address to these alerts and reports.\nUnsubscribe from these emails: ${url}\n`,
  };
}

/** Sends to every recipient not unsubscribed; returns how many were sent. */
export async function sendAll(deps: SendDeps, out: Outgoing): Promise<{ sent: number; failed: number }> {
  const unsubscribed = new Set(
    await deps.db.asProject(out.projectId, async (c) =>
      (await c.query<{ email: string }>("select email from pa.mail_unsubscribes where project_id = $1 and email = any($2::text[])", [out.projectId, out.recipients.map((r) => r.toLowerCase())])).rows.map((r) => r.email),
    ),
  );
  let sent = 0;
  let failed = 0;
  for (const to of out.recipients) {
    let status: "sent" | "failed" | "unsubscribed" | "not_configured";
    let messageId: string | null = null;
    let error: string | null = null;
    if (unsubscribed.has(to.toLowerCase())) status = "unsubscribed";
    else if (!deps.secret || !deps.mailer.configured) status = "not_configured";
    else {
      const url = unsubscribeUrl(deps.publicUrl, deps.secret, out.projectId, to);
      const f = footer(url);
      const r = await deps.mailer.send({ to, project: out.projectId, subject: out.subject, html: out.html + f.html, text: out.text + f.text, idempotencyKey: `${out.idempotencyKey}:${to.toLowerCase()}`, unsubscribeUrl: url });
      status = r.status;
      if (r.status === "sent") messageId = r.id;
      if (r.status === "failed") error = r.error;
    }
    if (status === "sent") sent += 1;
    if (status === "failed") failed += 1;
    await deps.db.asProject(out.projectId, (c) =>
      c.query("insert into pa.mail_log (project_id, source, source_id, recipient, status, message_id, error) values ($1, $2, $3, $4, $5, $6, $7)", [out.projectId, out.source, out.sourceId, to, status, messageId, error]),
    );
  }
  return { sent, failed };
}

export async function unsubscribe(db: Db, projectId: string, email: string): Promise<void> {
  await db.asProject(projectId, (c) => c.query("insert into pa.mail_unsubscribes (project_id, email) values ($1, $2) on conflict do nothing", [projectId, email.toLowerCase()]));
}

export async function mailLog(db: Db, projectId: string, source: "alert" | "subscription", sourceId: string, limit = 50) {
  return db.asProject(projectId, async (c) =>
    (
      await c.query<{ recipient: string; status: string; message_id: string | null; error: string | null; created_at: Date }>(
        "select recipient, status, message_id, error, created_at from pa.mail_log where project_id = $1 and source = $2 and source_id = $3 order by created_at desc limit $4",
        [projectId, source, sourceId, limit],
      )
    ).rows.map((r) => ({ ...r, created_at: new Date(r.created_at).toISOString() })),
  );
}
