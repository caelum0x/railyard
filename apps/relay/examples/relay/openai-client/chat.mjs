// Call Relay with the official OpenAI Node SDK.
//
//   npm install openai
//   node chat.mjs
//
// Relay must be running (docker compose -f docker-compose.quickstart.yml up).
// Set RELAY_API_KEY only when the gateway has `[gateway.auth] enabled = true`.

import OpenAI from "openai";

const baseURL = `${process.env.RELAY_URL ?? "http://localhost:3000"}/openai/v1`;
// The quickstart gateway has auth off, so any non-empty string works here.
const apiKey = process.env.RELAY_API_KEY ?? "relay-quickstart";

const client = new OpenAI({ baseURL, apiKey });

const response = await client.chat.completions.create({
  model: "tensorzero::function_name::chat",
  messages: [{ role: "user", content: "Say hello in five words." }],
});
console.log("function chat:", response.choices[0]?.message.content);

const stream = await client.chat.completions.create({
  model: "tensorzero::model_name::gpt-4o-mini",
  messages: [{ role: "user", content: "Count from 1 to 5." }],
  stream: true,
});
process.stdout.write("stream: ");
for await (const chunk of stream) {
  process.stdout.write(chunk.choices[0]?.delta?.content ?? "");
}
process.stdout.write("\n");
