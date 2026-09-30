# @railyard/insights-server

Railyard Insights from your server.

```ts
import { createInsightsServer } from '@railyard/insights-server';

const insights = createInsightsServer({
  site: process.env.RAILYARD_SITE_KEY,          // to send (public)
  collector: process.env.RAILYARD_COLLECTOR_URL,
  token: process.env.RAILYARD_TOKEN,            // to read, export and delete (secret)
  app: 'shop',
});

await insights.identify(user.id, { plan: 'pro' });
await insights.track(user.id, 'invoice_paid', { amount: 49 });

const cohort = await insights.cohorts.save('Active pro', {
  performed: { event: 'created_project', min_times: 2, within_days: 30 },
  trait: { key: 'plan', equals: 'pro' },
});
const { members, people } = await insights.cohorts.members(cohort.cohort_id);
const byPlan = await insights.retention({ property: 'trait.plan', period: 'week' });
const topPages = await insights.breakdown({ dimension: 'path', days: 7 });
const csv = await insights.exportEvents({ format: 'csv' });   // up to 31 days
await insights.deleteUser(user.id);                          // done at once, or within 24 hours
```

| Call | What it gives |
|---|---|
| `identify(userId, traits)` | Names a person; traits merge into their profile |
| `track(userId, name, data)` | A custom event by that person |
| `profiles({ user })` | People with first and last seen, traits and sessions; with `user`, their recent events |
| `cohorts.save / list / members / remove` | Saved cohorts by what people did, did not do, a trait, or when they were first seen; counts are live |
| `retention({ property, period })` | For each value of `country`, `language`, `screen` or `trait.<key>`: people in period 0 and how many came back in each later period |
| `breakdown({ dimension })` | Events and visitors by `path`, `referrer`, `country`, `language`, `screen`, `event`, `hostname`, `data.<key>` or `trait.<key>` |
| `exportEvents / exportProfiles` | JSON rows or CSV text; spreadsheet formulas in cells are defused |
| `deleteUser(userId)` | Removes every event, session and trait of that person |

`token` is the org token from `railyard login`. Keep it in your secret store;
this package never prints it. No dependencies.
