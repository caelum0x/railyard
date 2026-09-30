# @railyard/analytics-js

Event analytics for web and Expo apps on Railyard. After `scribase add analytics`
the site key is already in your environment, so this is all you add:

```ts
import { createAnalytics } from "@railyard/analytics-js";

export const analytics = createAnalytics();

// When your consent banner says yes:
analytics.setConsent("granted");

analytics.capture("checkout_started", { items: 3 });
analytics.identify("user_123", { email: "ada@example.com" });
analytics.group("company", "acme", { name: "Acme" });
```

In Expo, import from `@railyard/analytics-js/react-native` (the package picks
it automatically in React Native bundlers):

```tsx
import { useEffect } from "react";
import { usePathname } from "expo-router";
import { createNativeAnalytics, screenTracker } from "@railyard/analytics-js/react-native";

export const analytics = createNativeAnalytics();
const track = screenTracker(analytics);

export function useScreens() {
  const path = usePathname();
  useEffect(() => void track(path), [path]);
}
```

## Where the key and host come from

| Framework | Site key | Host (optional) |
|---|---|---|
| Next.js | `NEXT_PUBLIC_ANALYTICS_SITE` | `NEXT_PUBLIC_ANALYTICS_HOST` |
| Expo | `EXPO_PUBLIC_ANALYTICS_SITE` | `EXPO_PUBLIC_ANALYTICS_HOST` |
| Vite | `VITE_ANALYTICS_SITE` | `VITE_ANALYTICS_HOST` |

You can also pass `{ site, host }` directly.

## Privacy defaults

- Consent first: until `setConsent("granted")` nothing is loaded, stored on the
  device or sent. Calls made before then are dropped, not queued. Pass
  `{ requireConsent: false }` only where your users need no consent.
- `setConsent("denied")` stops sending and forgets the visitor on this device.
- Recordings, when your project turns them on, mask every input, mask text
  inside `[data-private]` or `.ph-mask`, leave out anything inside
  `[data-block]`, and never record canvas, request headers or bodies.
- No console output and no performance timings are captured.
- The service stores no IP address.
- On mobile the queue is kept on the device, so events made offline are sent
  once the app is back online, and each event is stored once.

## Server side

Use `@railyard/analytics-node` for events from your server.

The underlying open-source SDKs and their licences are listed in `NOTICE`.
