# @railyard/openfeature

Railyard Flags for web, Next.js and Expo apps (Wave P PY2.1). No dependencies.

- `createFlagsClient({ key })`: reads one environment's flags with its public
  flags key (`ry_flags_...`, written to your env file when flags are turned on
  for the project) and polls every 30 s. Reads are cached 15 s, so a flip reaches the app within
  60 s with no redeploy. Pass `storage` (localStorage or AsyncStorage) to start
  with the last flags when the app opens offline.
- `RailyardFlagsProvider`: an OpenFeature client provider (browser, Expo). Pass
  `events: new OpenFeatureEventEmitter()` from your OpenFeature SDK to get
  configuration-changed events.
- `RailyardServerFlagsProvider`: an OpenFeature server provider for Next.js route
  handlers and server components; keeps each answer 15 s.

A flag is on or off with an optional text value. String, number and object
resolvers read the value (object parses JSON) and fall back when the flag is off.

Examples: `examples/expo/App.tsx`, `examples/next/app/page.tsx`.

This package has no install step in the Railyard workspace (it has no
dependencies); run its tests with `node --test test/*.test.mjs`.
