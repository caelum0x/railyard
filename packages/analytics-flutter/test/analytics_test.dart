// Dependency-free checks: `dart run test/analytics_test.dart` exits non-zero
// on a failure (no package:test, so it runs offline).
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:railyard_analytics/railyard_analytics.dart';

int passed = 0;
final failures = <String>[];

Future<void> check(String name, FutureOr<void> Function() body) async {
  try {
    await body();
    passed++;
    print('ok   $name');
  } catch (e) {
    failures.add('$name: $e');
    print('FAIL $name: $e');
  }
}

void expect(bool cond, String Function() message) {
  if (!cond) throw StateError(message());
}

class FakeTransport implements Transport {
  final requests = <(Uri, Map<String, dynamic>)>[];
  final statuses = <int>[];
  String flagsBody = '{}';
  bool offline = false;

  @override
  Future<HttpResult> post(Uri url, String json) async {
    if (offline) throw const SocketException('offline');
    requests.add((url, (jsonDecode(json) as Map).cast<String, dynamic>()));
    if (url.path.endsWith('/flags/')) return HttpResult(200, flagsBody);
    return HttpResult(statuses.isEmpty ? 200 : statuses.removeAt(0), '{"status":1}');
  }
}

Analytics client(Transport t, {IdentityStorage? storage, int flushAt = 100, int maxQueue = 1000}) => Analytics(
      AnalyticsConfig(site: 'site_123', host: 'https://pa.test/', flushAt: flushAt, flushInterval: Duration.zero, maxQueue: maxQueue),
      transport: t,
      storage: storage ?? MemoryStorage(),
    );

List<Map<String, dynamic>> batchOf((Uri, Map<String, dynamic>) r) => (r.$2['batch'] as List).cast<Map<String, dynamic>>();

Future<void> main() async {
  await check('capture batches with the site key and library properties', () async {
    final t = FakeTransport();
    final a = client(t);
    a.capture('signed_up', {'plan': 'pro'});
    a.screen('Home');
    expect(await a.flush(), () => 'flush');
    expect(t.requests.length == 1 && t.requests[0].$1.toString() == 'https://pa.test/batch/', () => '${t.requests}');
    expect(t.requests[0].$2['api_key'] == 'site_123', () => 'api key');
    final batch = batchOf(t.requests[0]);
    expect(batch.map((e) => e['event']).join(',') == r'signed_up,$screen', () => '$batch');
    final props = batch[0]['properties'] as Map;
    expect(props['plan'] == 'pro' && props[r'$lib'] == 'railyard-analytics-flutter', () => '$props');
    expect((batch[1]['properties'] as Map)[r'$screen_name'] == 'Home', () => 'screen name');
    expect(RegExp(r'^[0-9a-f]{8}-[0-9a-f]{4}-4').hasMatch(batch[0]['uuid'] as String), () => 'uuid v4');
    expect(a.queuedCount == 0, () => 'drained');
  });

  await check('identify links the anonymous id and persists; reset forgets', () async {
    final t = FakeTransport();
    final storage = MemoryStorage();
    final a = client(t, storage: storage);
    final anon = a.distinctId;
    a.identify('user_42', set: {'email': 'a@b.c'});
    a.capture('clicked');
    await a.flush();
    final batch = batchOf(t.requests[0]);
    expect(batch[0]['event'] == r'$identify' && batch[0]['distinct_id'] == 'user_42', () => '$batch');
    final props = batch[0]['properties'] as Map;
    expect(props[r'$anon_distinct_id'] == anon && (props[r'$set'] as Map)['email'] == 'a@b.c', () => '$props');
    expect(batch[1]['distinct_id'] == 'user_42', () => 'later events use the id');
    final b = client(t, storage: storage);
    expect(b.distinctId == 'user_42', () => 'persisted');
    b.reset();
    expect(b.distinctId != 'user_42' && b.distinctId != anon, () => 'fresh anonymous id');
  });

  await check('server errors keep events, client errors drop them', () async {
    final t = FakeTransport()..statuses.addAll([503, 200]);
    final a = client(t);
    a.capture('one');
    expect(!(await a.flush()) && a.queuedCount == 1, () => 'kept for retry');
    expect(await a.flush() && a.queuedCount == 0, () => 'sent on retry');
    t.statuses.add(400);
    a.capture('bad');
    await a.flush();
    expect(a.queuedCount == 0, () => 'a 400 never succeeds');
    t.offline = true;
    a.capture('offline');
    expect(!(await a.flush()) && a.queuedCount == 1, () => 'kept while offline');
  });

  await check('flushAt sends without a call and the queue is bounded', () async {
    final t = FakeTransport();
    final a = client(t, flushAt: 2, maxQueue: 3);
    a.capture('a');
    a.capture('b');
    for (var i = 0; i < 50 && t.requests.isEmpty; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
    }
    expect(t.requests.length == 1, () => 'auto flush: ${t.requests.length}');
    t.offline = true;
    for (var i = 0; i < 10; i++) {
      a.capture('e$i');
    }
    expect(a.queuedCount <= 3, () => 'bounded: ${a.queuedCount}');
  });

  await check('feature flags and groups', () async {
    final t = FakeTransport()
      ..flagsBody = jsonEncode({
        'flags': {
          'new-ui': {'key': 'new-ui', 'enabled': true, 'variant': 'b'},
          'beta': {'key': 'beta', 'enabled': true, 'variant': null},
          'off': {'key': 'off', 'enabled': false},
        }
      });
    final a = client(t);
    a.group('company', 'acme');
    expect(await a.reloadFeatureFlags(), () => 'loaded');
    final req = t.requests.last;
    expect(req.$1.path.endsWith('/flags/') && (req.$2['groups'] as Map)['company'] == 'acme', () => '$req');
    expect(a.featureFlag('new-ui') == 'b' && a.featureFlag('beta') == 'true', () => 'variants');
    expect(!a.isFeatureEnabled('off') && !a.isFeatureEnabled('nope'), () => 'off flags');
    a.capture('after_group');
    await a.flush();
    final last = batchOf(t.requests.last).last['properties'] as Map;
    expect((last[r'$groups'] as Map)['company'] == 'acme', () => '$last');
  });

  await check('bad configuration is refused', () {
    var refused = 0;
    for (final cfg in [AnalyticsConfig(site: ' '), AnalyticsConfig(site: 's', host: 'ftp://x')]) {
      try {
        Analytics(cfg, transport: FakeTransport());
      } on ArgumentError {
        refused++;
      }
    }
    expect(refused == 2, () => 'refused $refused');
  });

  await check('the dart:io transport talks to a real HTTP server', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final received = <String>[];
    server.listen((req) async {
      received.add('${req.method} ${req.uri.path} ${await utf8.decoder.bind(req).join()}');
      req.response
        ..statusCode = 200
        ..write('{"status":1}');
      await req.response.close();
    });
    final transport = IoTransport();
    final a = Analytics(AnalyticsConfig(site: 'live', host: 'http://127.0.0.1:${server.port}', flushInterval: Duration.zero), transport: transport);
    a.capture('ping', {'n': 1});
    expect(await a.flush(), () => 'flush over HTTP');
    expect(received.length == 1 && received[0].startsWith('POST /batch/ ') && received[0].contains('"api_key":"live"'), () => '$received');
    transport.close();
    await server.close(force: true);
  });

  await check('file storage persists ids across clients', () {
    final file = File('${Directory.systemTemp.path}/railyard-analytics-${DateTime.now().microsecondsSinceEpoch}.json');
    final a = client(FakeTransport(), storage: FileStorage(file));
    a.identify('persisted');
    expect(client(FakeTransport(), storage: FileStorage(file)).distinctId == 'persisted', () => 'read back');
    file.deleteSync();
  });

  print('$passed passed, ${failures.length} failed');
  exit(failures.isEmpty ? 0 : 1);
}
