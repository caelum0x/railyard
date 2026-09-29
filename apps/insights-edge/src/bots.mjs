// Drop known bots, crawlers, link previewers and prefetches before counting.

const BOT_UA = /bot\b|crawler|spider|crawling|slurp|headless|phantomjs|puppeteer|playwright|selenium|lighthouse|pagespeed|pingdom|uptime|monitor|curl\/|wget\/|python-requests|python-urllib|go-http-client|okhttp|java\/|libwww|httpclient|axios\/|node-fetch|undici|facebookexternalhit|embedly|preview|whatsapp|slackbot|discordbot|telegrambot|vercelbot|google-inspectiontool|bingpreview|yandex|baiduspider|duckduckbot|applebot|petalbot|semrush|ahrefs|mj12bot|dotbot|bytespider|gptbot|claudebot|ccbot|perplexity/i;

/** @returns {null | "bot" | "prefetch"} why the request is dropped, or null to keep it */
export function dropReason(headers) {
  const purpose = `${headers.get("purpose") ?? ""} ${headers.get("sec-purpose") ?? ""} ${headers.get("x-purpose") ?? ""} ${headers.get("x-moz") ?? ""}`.toLowerCase();
  if (/prefetch|preview|prerender/.test(purpose)) return "prefetch";
  const ua = headers.get("user-agent") ?? "";
  if (!ua || ua.length < 12 || BOT_UA.test(ua)) return "bot";
  return null;
}
