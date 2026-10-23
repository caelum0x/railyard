// An agent failure is three fields: who failed, which model, and the error name.
// A page view has no name. Any other name is a custom event. A prompt, a key,
// or any other field is dropped and never returned.

export const AGENT_FAILURE_EVENT = "$agent_failure";

const FIELD = /^[A-Za-z0-9][A-Za-z0-9 ._:/-]{0,79}$/;

function field(value) {
  if (typeof value !== "string") return null;
  const text = value.trim().replace(/[ \t]+/g, " ");
  if (!FIELD.test(text)) return null;
  if (/(sk-|api[_-]?key|bearer\s|password|secret|token|BEGIN )/i.test(text)) return null;
  return text;
}

/** The stored record, or null when this event is not an agent failure. */
export function agentFailureRecord(event) {
  if (!event || event.name !== AGENT_FAILURE_EVENT) return null;
  const data = event.data && typeof event.data === "object" && !Array.isArray(event.data) ? event.data : {};
  const agent = field(data.agent);
  const error = field(data.error);
  if (!agent || !error) return null;
  const model = field(data.model);
  return model ? { agent, model, error } : { agent, error };
}

/** GlitchTip (Sentry store) issue. The exception type is the error name. No prompt. */
export function faultlineIssue(record, { eventId, timestamp }) {
  return {
    event_id: eventId,
    timestamp,
    platform: "other",
    level: "error",
    logger: "agent",
    exception: { values: [{ type: record.error, value: record.agent }] },
    tags: { kind: "agent_failure", agent: record.agent, ...(record.model ? { model: record.model } : {}) },
    fingerprint: ["agent_failure", record.agent, record.error],
  };
}

/** `https://<key>@host/<project>` to the store URL, or null. */
export function faultlineStoreUrl(dsn) {
  if (!dsn || typeof dsn !== "string") return null;
  let url;
  try {
    url = new URL(dsn.trim());
  } catch {
    return null;
  }
  const key = url.username;
  const project = url.pathname.replace(/^\/+|\/+$/g, "");
  if (!key || !/^\d+$/.test(project) || url.protocol !== "https:") return null;
  const query = new URLSearchParams({ sentry_key: key, sentry_version: "7" });
  return `${url.protocol}//${url.host}/api/${project}/store/?${query.toString()}`;
}

function eventId() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Posts each agent failure in `batches` to Faultline. A refusal is logged by
 * the caller. The body is only the cleaned record.
 * @returns {"skip" | "sent" | "drop"}
 */
export async function sendAgentFailures(dsn, batches, now) {
  const url = faultlineStoreUrl(dsn);
  if (!url) return "skip";
  const rows = [];
  for (const batch of batches) for (const event of batch.events) {
    const rec = agentFailureRecord(event);
    if (rec) rows.push(rec);
  }
  if (!rows.length) return "skip";
  const timestamp = new Date(now).toISOString();
  let failed = false;
  for (const rec of rows) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "content-type": "text/plain;charset=UTF-8" },
        body: JSON.stringify(faultlineIssue(rec, { eventId: eventId(), timestamp })),
      });
      if (!res.ok) failed = true;
    } catch {
      failed = true;
    }
  }
  return failed ? "drop" : "sent";
}
