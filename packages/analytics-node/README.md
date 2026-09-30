# @railyard/analytics-node

Server-side events for apps on Railyard.

```ts
import { createServerAnalytics } from "@railyard/analytics-node";

const analytics = createServerAnalytics(); // reads ANALYTICS_SITE or NEXT_PUBLIC_ANALYTICS_SITE

analytics.capture("user_123", "invoice_paid", { amount_cents: 1900 }, { company: "acme" });
analytics.identify("user_123", { plan: "pro" }, { first_plan: "free" });
analytics.groupIdentify("company", "acme", { name: "Acme" });

if (await analytics.isEnabled("new_checkout", "user_123")) {
  // ...
}

await analytics.shutdown(); // sends what is still queued
```

| Setting | Default |
|---|---|
| `site` | `ANALYTICS_SITE`, then `NEXT_PUBLIC_ANALYTICS_SITE` |
| `host` | `PRODUCT_ANALYTICS_URL`, then `ANALYTICS_HOST`, then the Railyard host |

Events are sent in batches of 20 or every 10 seconds. A batch that is sent
twice (for example after a timeout) is stored once. The service stores no IP
address.

The underlying open-source SDK and its licence are listed in `NOTICE`.
