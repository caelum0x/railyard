// Alert email through Rally Send (Wave E contract 5.2): our own transactional
// sender, product "railyard". RALLY_TX_URL and RALLY_TX_KEY on the console.

export interface Mailer {
  send(msg: { to: string; subject: string; text: string; html: string; idempotencyKey: string; project: string }): Promise<boolean>;
}

type Fetch = typeof fetch;

export function rallySend(opts: { url: string; key: string; fetchImpl?: Fetch }): Mailer {
  return {
    async send(msg) {
      const res = await (opts.fetchImpl ?? fetch)(opts.url, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${opts.key}` },
        body: JSON.stringify({
          product: "railyard",
          project: msg.project,
          to: msg.to,
          from_local: "status",
          from_name: "Railyard status",
          subject: msg.subject,
          text: msg.text,
          html: msg.html,
          idempotency_key: msg.idempotencyKey,
          tags: ["watch"],
        }),
      });
      if (res.status === 202) return true;
      console.error(`watch alert not sent: Rally Send answered ${res.status}`);
      return false;
    },
  };
}

/** No sender configured: alerts are logged, never lost silently. */
export const noMailer: Mailer = {
  async send(msg) {
    console.error(`watch alert not sent (no RALLY_TX_URL): ${msg.subject}`);
    return false;
  },
};
