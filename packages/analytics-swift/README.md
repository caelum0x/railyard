# RailyardAnalytics (Swift)

Native iOS, macOS, tvOS and watchOS client for Railyard product analytics. It
speaks the service's PostHog-compatible wire protocol (`/batch/`, `/flags/`).

```swift
import RailyardAnalytics

let analytics = try Analytics(config: AnalyticsConfig(site: "<site key>"))
await analytics.capture("signed_up", properties: ["plan": "pro"])
await analytics.screen("Home")
await analytics.identify("user_42", set: ["email": "ada@example.com"])
await analytics.group("company", key: "acme")
if await analytics.reloadFeatureFlags(), await analytics.isFeatureEnabled("new-ui") { ... }
await analytics.flush()   // also runs every `flushInterval` seconds and at `flushAt` events
await analytics.reset()   // on log out
```

- Events queue in memory (bounded by `maxQueue`) and go out in batches of up
  to 100. Network errors, 429 and 5xx keep them for the next flush; other 4xx
  answers drop them.
- The anonymous id and the identified id persist in `UserDefaults` (pass
  `MemoryStorage()` to keep nothing on disk).
- `Transport` is a protocol, so tests and apps can swap the HTTP layer.

Add it with Swift Package Manager (path or git URL of `packages/analytics-swift`).
Run the tests with `swift test`.
