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

## Rules and gradual release

A flag can carry ordered rules. Each rule has conditions (all must match) and
a share of the matching people (0 to 100 percent, on a stable hash of the
flag key and the person's id, so a person keeps their answer and raising the
share only adds people). The first matching rule decides, and may set its own
value. Conditions compare a person's property (is, is not, contains, matches a
pattern, greater or less than, is set) or name a segment: a named set of
property conditions kept once per environment.

The providers evaluate rules locally with the same code the server runs. Pass
the person in the OpenFeature evaluation context: `targetingKey` is their id
and the other attributes are their properties:

```ts
await OpenFeature.setContext({ targetingKey: user.id, plan: user.plan, country: "DE" });
client.getBooleanValue("new_checkout", false);
```

`client.evaluate(key, { id, properties })` does the same on the plain client.
Without a person, a flag with rules reads as off.

Examples: `examples/expo/App.tsx`, `examples/next/app/page.tsx`.

This package has no install step in the Railyard workspace (it has no
dependencies); run its tests with `node --test test/*.test.mjs`.
