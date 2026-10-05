// Mail for alerts and subscriptions (QY1.4) through the Rally Send
// transactional contract (Wave E 5.2): POST {product, project, to,
// from_local, subject, html, text, idempotency_key, unsubscribe_url, tags}
// with the product's key; 202 {id}, the same id for a repeated idempotency
// key, 429 over the project's cap, 422 for a suppressed address. Without
// RALLY_TX_URL this service has no other mail path: the mail is logged as
// not_configured, never dropped silently.

export interface Mail {
  to: string;
  project: string;
  subject: string;
  html: string;
  text: string;
  idempotencyKey: string;
  unsubscribeUrl: string;
}

export type MailOutcome = { status: "sent"; id: string } | { status: "failed"; error: string } | { status: "not_configured" };

export interface Mailer {
  readonly configured: boolean;
  send(mail: Mail): Promise<MailOutcome>;
}

export function rallyMailer(opts: { url: string; key: string; fetchImpl?: typeof fetch }): Mailer {
  return {
    configured: true,
    async send(m) {
      let res: Response;
      try {
        res = await (opts.fetchImpl ?? fetch)(opts.url, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${opts.key}` },
          signal: AbortSignal.timeout(10_000),
          body: JSON.stringify({
            product: "railyard",
            project: m.project,
            to: m.to,
            from_local: "insights",
            subject: m.subject,
            html: m.html,
            text: m.text,
            idempotency_key: m.idempotencyKey,
            unsubscribe_url: m.unsubscribeUrl,
            tags: ["product-analytics"],
          }),
        });
      } catch (err) {
        return { status: "failed", error: `The mail service could not be reached: ${(err as Error).message}`.slice(0, 300) };
      }
      const body = (await res.json().catch(() => ({}))) as { id?: unknown };
      if (res.status === 202 && typeof body.id === "string") return { status: "sent", id: body.id };
      if (res.status === 422) return { status: "failed", error: "The address is suppressed (an earlier bounce or complaint)." };
      if (res.status === 429) return { status: "failed", error: "Today's mail cap for this project is reached." };
      return { status: "failed", error: `The mail service answered ${res.status}.` };
    },
  };
}

export const noMailer: Mailer = {
  configured: false,
  async send() {
    return { status: "not_configured" };
  },
};

/** RALLY_TX_URL (https) and RALLY_TX_KEY, else no mailer. */
export function mailerFromEnv(env: Record<string, string | undefined> = process.env): Mailer {
  const url = env.RALLY_TX_URL?.trim();
  const key = env.RALLY_TX_KEY?.trim();
  return url?.startsWith("https://") && key ? rallyMailer({ url, key }) : noMailer;
}

export const EMAIL = /^[^\s@<>"',;]{1,64}@[^\s@<>"',;]{1,253}\.[A-Za-z]{2,63}$/;

export const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch]!);
