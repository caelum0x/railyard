import 'dart:async';
import 'dart:convert';
import 'dart:io' show Platform;
import 'dart:math';

import 'transport.dart';

/// Configuration. [site] is the app's public site key.
class AnalyticsConfig {
  AnalyticsConfig({
    required this.site,
    this.host = defaultHost,
    int flushAt = 20,
    this.flushInterval = const Duration(seconds: 10),
    int maxQueue = 1000,
  })  : flushAt = max(1, flushAt),
        maxQueue = max(1, maxQueue);

  static const defaultHost = 'https://pa.railyard.ereboslab.com';

  final String site;
  final String host;

  /// Send when this many events are queued.
  final int flushAt;

  /// Also send on this interval; `Duration.zero` disables the timer.
  final Duration flushInterval;

  /// Oldest events are dropped beyond this many queued.
  final int maxQueue;
}

/// A feature flag as the service evaluated it.
class FeatureFlag {
  const FeatureFlag(this.key, this.enabled, this.variant);
  final String key;
  final bool enabled;
  final String? variant;
}

/// Client for Railyard product analytics (PostHog-compatible wire protocol):
/// capture, screens, identify, alias, groups, super properties and feature
/// flags. Events queue in memory and go out in batches.
class Analytics {
  Analytics(this.config, {Transport? transport, IdentityStorage? storage, DateTime Function()? clock})
      : _transport = transport ?? IoTransport(),
        _storage = storage ?? MemoryStorage(),
        _clock = clock ?? DateTime.now {
    final site = config.site.trim();
    if (site.isEmpty || site.length > 200) {
      throw ArgumentError('site must be a site key of 1 to 200 characters');
    }
    final uri = Uri.tryParse(config.host);
    if (uri == null || !(uri.scheme == 'https' || uri.scheme == 'http') || uri.host.isEmpty) {
      throw ArgumentError('host must be an http(s) URL');
    }
    _base = config.host.endsWith('/') ? config.host.substring(0, config.host.length - 1) : config.host;
    if (config.flushInterval > Duration.zero) {
      _timer = Timer.periodic(config.flushInterval, (_) => flush());
    }
  }

  static const libName = 'railyard-analytics-flutter';
  static const libVersion = '0.1.0';
  static const anonKey = 'railyard.analytics.anonymous_id';
  static const distinctKey = 'railyard.analytics.distinct_id';
  static const _batchSize = 100;

  final AnalyticsConfig config;
  final Transport _transport;
  final IdentityStorage _storage;
  final DateTime Function() _clock;
  late final String _base;
  Timer? _timer;
  final List<Map<String, Object?>> _queue = [];
  final Map<String, Object?> _superProperties = {};
  final Map<String, String> _groups = {};
  Map<String, FeatureFlag> _flags = {};
  Future<bool>? _flushing;
  static final Random _random = Random.secure();

  /// The identified id, else a persisted anonymous id.
  String get distinctId => _storage.getString(distinctKey) ?? _anonymousId();

  String _anonymousId() {
    final existing = _storage.getString(anonKey);
    if (existing != null) return existing;
    final id = _uuid();
    _storage.setString(anonKey, id);
    return id;
  }

  static String _uuid() {
    final b = List<int>.generate(16, (_) => _random.nextInt(256));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    final h = b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();
    return '${h.substring(0, 8)}-${h.substring(8, 12)}-${h.substring(12, 16)}-${h.substring(16, 20)}-${h.substring(20)}';
  }

  /// Events waiting to be sent.
  int get queuedCount => _queue.length;

  /// Properties sent with every event.
  void register(Map<String, Object?> properties) => _superProperties.addAll(properties);

  void capture(String event, [Map<String, Object?> properties = const {}]) {
    if (event.isEmpty || event.length > 200) return;
    final props = <String, Object?>{
      r'$lib': libName,
      r'$lib_version': libVersion,
      r'$os': _os(),
      ..._superProperties,
      if (_groups.isNotEmpty) r'$groups': Map<String, String>.of(_groups),
      ...properties,
    };
    _queue.add({
      'event': event,
      'distinct_id': distinctId,
      'properties': props,
      'timestamp': _clock().toUtc().toIso8601String(),
      'uuid': _uuid(),
    });
    if (_queue.length > config.maxQueue) {
      _queue.removeRange(0, _queue.length - config.maxQueue);
    }
    if (_queue.length >= config.flushAt) {
      unawaited(flush());
    }
  }

  static String _os() {
    try {
      return Platform.operatingSystem;
    } on UnsupportedError {
      return 'web';
    }
  }

  /// A screen view (`$screen`).
  void screen(String name, [Map<String, Object?> properties = const {}]) => capture(r'$screen', {...properties, r'$screen_name': name});

  /// Tie the current anonymous user to [id] and set person properties.
  void identify(String id, {Map<String, Object?> set = const {}, Map<String, Object?> setOnce = const {}}) {
    if (id.isEmpty) return;
    final previous = distinctId;
    _storage.setString(distinctKey, id);
    capture(r'$identify', {
      r'$anon_distinct_id': previous,
      if (set.isNotEmpty) r'$set': set,
      if (setOnce.isNotEmpty) r'$set_once': setOnce,
    });
  }

  /// Another id for the current user.
  void alias(String alias) => capture(r'$create_alias', {'alias': alias});

  /// Associate later events with a group and set its properties.
  void group(String type, String key, [Map<String, Object?> properties = const {}]) {
    _groups[type] = key;
    capture(r'$groupidentify', {r'$group_type': type, r'$group_key': key, if (properties.isNotEmpty) r'$group_set': properties});
  }

  /// Forget the user (log out): new anonymous id, no groups, no flags.
  void reset() {
    _storage.setString(distinctKey, null);
    _storage.setString(anonKey, null);
    _groups.clear();
    _flags = {};
  }

  /// Send everything queued. Network errors, 429 and 5xx keep the events for
  /// the next flush; other 4xx answers drop them. Concurrent calls share one run.
  Future<bool> flush() => _flushing ??= _flush().whenComplete(() => _flushing = null);

  Future<bool> _flush() async {
    while (_queue.isNotEmpty) {
      final batch = _queue.take(_batchSize).toList();
      final body = jsonEncode({'api_key': config.site, 'batch': batch, 'sent_at': _clock().toUtc().toIso8601String()});
      int status;
      try {
        status = (await _transport.post(Uri.parse('$_base/batch/'), body)).status;
      } catch (_) {
        return false;
      }
      if (status == 429 || status >= 500) return false;
      _queue.removeRange(0, min(batch.length, _queue.length));
      if (status < 200 || status >= 300) return false;
    }
    return true;
  }

  /// Fetch the flags for the current user.
  Future<bool> reloadFeatureFlags({Map<String, Object?> personProperties = const {}}) async {
    final body = jsonEncode({
      'api_key': config.site,
      'distinct_id': distinctId,
      if (personProperties.isNotEmpty) 'person_properties': personProperties,
      if (_groups.isNotEmpty) 'groups': _groups,
    });
    try {
      final result = await _transport.post(Uri.parse('$_base/flags/'), body);
      if (result.status < 200 || result.status >= 300) return false;
      final decoded = jsonDecode(result.body);
      final list = decoded is Map ? decoded['flags'] : null;
      if (list is! Map) return false;
      _flags = {
        for (final entry in list.entries)
          if (entry.value is Map)
            entry.key as String: FeatureFlag(entry.key as String, (entry.value as Map)['enabled'] == true, (entry.value as Map)['variant'] as String?),
      };
      return true;
    } catch (_) {
      return false;
    }
  }

  bool isFeatureEnabled(String key) => _flags[key]?.enabled ?? false;

  /// The variant of a multivariate flag, `"true"` for an enabled boolean flag, else null.
  String? featureFlag(String key) {
    final f = _flags[key];
    if (f == null || !f.enabled) return null;
    return f.variant ?? 'true';
  }

  /// Flush and stop the timer.
  Future<void> close() async {
    _timer?.cancel();
    await flush();
  }
}
