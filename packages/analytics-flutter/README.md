# railyard_analytics (Flutter / Dart)

Client for Railyard product analytics, at parity with the Swift and Kotlin
SDKs. It speaks the service's PostHog-compatible wire protocol (`/batch/`,
`/flags/`) and has no dependencies beyond the Dart SDK.

```dart
import 'package:railyard_analytics/railyard_analytics.dart';

final analytics = Analytics(AnalyticsConfig(site: '<site key>'), storage: prefsStorage);
analytics.capture('signed_up', {'plan': 'pro'});
analytics.screen('Home');
analytics.identify('user_42', set: {'email': 'ada@example.com'});
analytics.group('company', 'acme');
if (await analytics.reloadFeatureFlags() && analytics.isFeatureEnabled('new-ui')) { /* ... */ }
await analytics.flush();   // also every flushInterval and at flushAt events
analytics.reset();         // on log out
await analytics.close();   // flush and stop the timer
```

Persist ids in Flutter with a `SharedPreferences` adapter:

```dart
class PrefsStorage implements IdentityStorage {
  PrefsStorage(this.prefs);
  final SharedPreferences prefs;
  @override
  String? getString(String key) => prefs.getString(key);
  @override
  void setString(String key, String? value) => value == null ? prefs.remove(key) : prefs.setString(key, value);
}
```

The default transport uses `dart:io` (Android, iOS, desktop, servers). For
Flutter web, pass a `Transport` built on `package:http`.

Events queue in memory (bounded by `maxQueue`) and go out in batches of up to
100. Network errors, 429 and 5xx keep them for the next flush; other 4xx
answers drop them.

Add it with a path or git dependency on `packages/analytics-flutter`.
Tests: `dart run test/analytics_test.dart` (no packages needed).
