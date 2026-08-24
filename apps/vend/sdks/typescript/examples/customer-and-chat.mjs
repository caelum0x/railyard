import { randomUUID } from "node:crypto";
import { VendClient } from "../dist/index.js";

const required = [
  "VEND_BASE_URL",
  "VEND_TENANT_ID",
  "VEND_EMAIL",
  "VEND_PASSWORD",
  "VEND_PROVISIONING_KEY",
  "VEND_MODEL",
];
for (const name of required) {
  if (!process.env[name]) throw new Error(`missing ${name}`);
}

const vend = new VendClient({
  baseUrl: process.env.VEND_BASE_URL,
  provisioningKey: process.env.VEND_PROVISIONING_KEY,
});
await vend.login(
  process.env.VEND_TENANT_ID,
  process.env.VEND_EMAIL,
  process.env.VEND_PASSWORD,
);
const apps = await vend.listApps();
console.log(`authenticated; visible apps=${apps.length}`);

const completion = await vend.chatCompletions(
  {
    model: process.env.VEND_MODEL,
    messages: [{ role: "user", content: "Reply with exactly: Vend works" }],
    temperature: 0,
    max_tokens: 16,
  },
  process.env.VEND_IDEMPOTENCY_KEY ?? randomUUID(),
);
console.log(JSON.stringify(completion.body));
console.log(completion.settlement);

if (process.env.VEND_RUN_STREAM === "true") {
  const stream = await vend.chatCompletionsStream(
    {
      model: process.env.VEND_MODEL,
      messages: [{ role: "user", content: "Count from one to three" }],
      temperature: 0,
      max_tokens: 32,
    },
    process.env.VEND_STREAM_IDEMPOTENCY_KEY ?? randomUUID(),
  );
  for await (const event of stream) process.stdout.write(`${event.data}\n`);
}

await vend.logout();
