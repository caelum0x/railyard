# @railyard/insights-react-native

Railyard Insights for React Native and Expo apps: screens, events and the
signed-in person, sent from a queue on the device, so an app that was offline
sends everything once it is back, and each event only once.

```ts
import AsyncStorage from '@react-native-async-storage/async-storage';
import { createInsights } from '@railyard/insights-react-native';

const insights = createInsights({
  site: process.env.EXPO_PUBLIC_RAILYARD_SITE!,          // ry_site_..., public
  collector: process.env.EXPO_PUBLIC_RAILYARD_COLLECTOR!,
  storage: AsyncStorage,
  app: 'com.acme.shop',
});

insights.screen('/home');
await insights.identify(user.id, { plan: 'pro' });  // after sign-in
insights.track('created_project', { template: 'blank' });
await insights.reset();                              // after sign-out
```

- `identify` takes your app's own user id (1 to 50 of `A-Z a-z 0-9 @ . _ : + = -`)
  and up to 20 traits. The id is kept on the device, so later events, even
  after a restart, belong to that person until `reset`.
- Pass `consent: () => boolean`; while it returns false nothing is queued or sent.
- No IP address is read or sent by this package; the collector never stores one.
- No dependencies. Any storage with `getItem` and `setItem` works.

Profiles, cohorts, retention by property, breakdowns, export and delete by
user id are read on your server with `@railyard/insights-server`.
