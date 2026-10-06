# railyard-analytics-kotlin

Native Android and JVM client for Railyard product analytics. It speaks the
service's PostHog-compatible wire protocol (`/batch/`, `/flags/`) and depends
only on the Kotlin standard library (`HttpURLConnection`, no OkHttp).

```kotlin
val analytics = Analytics(AnalyticsConfig(site = "<site key>"), storage = prefsStorage)
analytics.capture("signed_up", mapOf("plan" to "pro"))
analytics.screen("Home")
analytics.identify("user_42", set = mapOf("email" to "ada@example.com"))
analytics.group("company", "acme")
if (analytics.reloadFeatureFlags() && analytics.isFeatureEnabled("new-ui")) { /* ... */ }
analytics.flush()   // blocking; also runs every flushIntervalSeconds and at flushAt events
analytics.reset()   // on log out
analytics.close()   // flush and stop the background thread
```

On Android, persist ids with a `SharedPreferences` adapter:

```kotlin
val prefs = context.getSharedPreferences("railyard_analytics", Context.MODE_PRIVATE)
val prefsStorage = object : IdentityStorage {
    override fun getString(key: String) = prefs.getString(key, null)
    override fun putString(key: String, value: String?) = prefs.edit().putString(key, value).apply()
}
```

Call `reloadFeatureFlags()` and `flush()` off the main thread (they block on
the network). Timestamps use `java.time`; Android needs API 26+ or core
library desugaring.

Events queue in memory (bounded by `maxQueue`) and go out in batches of up to
100. Network errors, 429 and 5xx keep them for the next flush; other 4xx
answers drop them.

Tests: `scripts/test.sh` (uses `kotlinc`, or the Kotlin compiler inside a
local Gradle distribution, so it runs offline). With Gradle: `gradle check`.
