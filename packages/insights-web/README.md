# @railyard/insights-web

Railyard Insights for web apps: page views (including client-side route
changes), custom events, identify, and five page-speed metrics for each page
view. No cookies and no storage; nothing is sent when Do Not Track is on or
when `consent()` returns false. No dependencies.

```ts
import { createInsightsWeb } from '@railyard/insights-web';

const insights = createInsightsWeb({
  site: 'ry_site_...',                    // public, like an anon key
  collector: 'https://collect.example',
});
insights.track('signup');
insights.identify(user.id, { plan: 'pro' });
```

## Page speed

Each page view reports, once, when the page is hidden or the route changes:

| Metric | What it is |
|---|---|
| LCP | when the largest text or image finished painting, up to the first click or key press |
| INP | how long the page took to respond to input (the slowest, skipping one per 50 inputs) |
| CLS | how much the layout moved (the worst burst of shifts) |
| FCP | when the first text or image painted |
| TTFB | when the server's first byte arrived |

The numbers follow the definitions Chrome uses for its own field reporting
(`computeVitals` in `src/metrics.ts`, tested against worked fixtures within
10%). Insights shows the 75th percentile per app and, for each deploy, how
each metric moved from the day before to the day after.

The one-line page tag (`/s.js`) sends page views and events only; use this
package to add page speed.

`NOTICE` lists the MIT-licensed source `src/metrics.ts` adapts.
