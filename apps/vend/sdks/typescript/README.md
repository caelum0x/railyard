# Vend TypeScript SDK

Dependency-free client for Node.js 20+ and modern browsers. It keeps customer
sessions (control plane) separate from provisioning keys (inference plane).

```bash
npm install
npm run build
```

```ts
import { VendClient } from "@reelforge/vend-sdk";

const vend = new VendClient({ baseUrl: process.env.VEND_BASE_URL! });
const session = await vend.login(
  process.env.VEND_TENANT_ID!,
  process.env.VEND_EMAIL!,
  process.env.VEND_PASSWORD!,
);
const apps = await vend.listApps();
const issued = await vend.issueKey(apps[0]!.id, {
  environment: "test",
  daily_cap_micros: 5_000_000,
});

vend.setProvisioningKey(issued.secret);
const reply = await vend.chatCompletions(
  { model: "tensorzero::model_name::openai::gpt-4o-mini", messages: [{ role: "user", content: "Hello" }] },
  crypto.randomUUID(),
);
console.log(reply.body, reply.settlement);

const stream = await vend.chatCompletionsStream(
  { model: "tensorzero::model_name::openai::gpt-4o-mini", messages: [{ role: "user", content: "Hello" }] },
  crypto.randomUUID(),
);
for await (const event of stream) console.log(event.data);
```

For an invited account, direct the browser to
`vend.oauthStartUrl("github", tenantId)` or
`vend.oauthStartUrl("google", tenantId)`. Vend owns state, PKCE, callback
validation, membership matching, and session issuance. OAuth is not an open
signup path.
