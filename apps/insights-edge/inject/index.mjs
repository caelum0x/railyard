// Railyard Insights tag injector: a pass-through Worker route in front of a
// site we cannot redeploy cheaply (box-hosted apps behind the tunnel). It adds
// the cookieless page tag to HTML responses and leaves everything else as is.
// Sites: hostname -> site key (SITE_KEYS var, JSON). Also fronts yokharita.com and
// oxhanatelier.com, whose apps ship the tag themselves but are redeployed from
// other working trees; the tag is added only when the page does not already have it.

export const TAG_SRC = "https://insights.ereboslab.com/s.js";

export function injectTag(response, siteKey) {
  const type = response.headers.get("content-type") ?? "";
  if (!siteKey || !type.includes("text/html") || response.status >= 300) return response;
  // Skip pages that already carry the tag (sites that ship it in their own layout).
  let present = false;
  const out = new HTMLRewriter()
    .on(`script[src="${TAG_SRC}"]`, {
      element() {
        present = true;
      },
    })
    .on("body", {
      element(el) {
        el.onEndTag((end) => {
          if (!present) end.before(`<script defer src="${TAG_SRC}" data-site="${siteKey}"></script>`, { html: true });
        });
      },
    })
    .transform(response);
  const csp = out.headers.get("content-security-policy");
  if (!csp) return out;
  const headers = new Headers(out.headers);
  const host = new URL(TAG_SRC).origin;
  headers.set(
    "content-security-policy",
    csp.replace(/(script-src[^;]*)/, `$1 ${host}`).replace(/(connect-src[^;]*)/, `$1 ${host}`),
  );
  return new Response(out.body, { status: out.status, statusText: out.statusText, headers });
}

export default {
  async fetch(request, env) {
    const response = await fetch(request);
    let keys = {};
    try {
      keys = JSON.parse(env.SITE_KEYS ?? "{}");
    } catch {
      return response;
    }
    return injectTag(response, keys[new URL(request.url).hostname]);
  },
};
