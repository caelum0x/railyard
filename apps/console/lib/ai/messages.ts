// Everything the AI gateway says to a developer (Wave P PY1.4), in one place
// so the copy lint reads exactly what is shown.

export const MESSAGES = {
  unauthorized_header: "Use the project's AI key as a Bearer token.",
  unauthorized_key: "This AI key is not valid.",
  addon: "The AI gateway comes with the Portfolio add-on, which is off. Your settings are kept.",
  json: "The body must be JSON.",
  json_object: "The body must be a JSON object.",
  stream: "Streaming answers are not available on this gateway yet; send the request without stream.",
  credentials: "Credentials are connected once on the connect page, never sent with a request.",
  day: "This key's daily token cap is reached; it resets at 00:00 UTC.",
  month: "This key's monthly token cap is reached; it resets on the 1st (UTC).",
  not_json: "The provider answered with something that is not JSON.",
  model: (providers: readonly string[]): string =>
    `Name the provider in the model, for example "anthropic/claude-sonnet-4-5". Providers: ${providers.join(", ")}.`,
  byok: (provider: string): string => `Connect your own ${provider} account first; this gateway never uses anyone else's.`,
} as const;
