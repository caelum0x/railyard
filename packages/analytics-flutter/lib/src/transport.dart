import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// One HTTP answer.
class HttpResult {
  const HttpResult(this.status, this.body);
  final int status;
  final String body;
}

/// How the SDK reaches the analytics service; swap it in tests (or on the web).
abstract class Transport {
  /// POST a JSON body. Throws on network failure.
  Future<HttpResult> post(Uri url, String json);
}

/// `dart:io` transport (Android, iOS, macOS, Windows, Linux, servers).
class IoTransport implements Transport {
  IoTransport({HttpClient? client, this.timeout = const Duration(seconds: 15)}) : _client = client ?? HttpClient();

  final HttpClient _client;
  final Duration timeout;

  @override
  Future<HttpResult> post(Uri url, String json) async {
    final request = await _client.postUrl(url).timeout(timeout);
    request.headers.contentType = ContentType.json;
    request.add(utf8.encode(json));
    final response = await request.close().timeout(timeout);
    final body = await response.transform(utf8.decoder).join().timeout(timeout);
    return HttpResult(response.statusCode, body);
  }

  void close() => _client.close(force: true);
}

/// Where the anonymous and identified ids persist. In Flutter, wrap
/// `SharedPreferences`.
abstract class IdentityStorage {
  String? getString(String key);
  void setString(String key, String? value);
}

/// In-memory storage (tests, or apps that must not persist ids).
class MemoryStorage implements IdentityStorage {
  final Map<String, String> _values = {};

  @override
  String? getString(String key) => _values[key];

  @override
  void setString(String key, String? value) {
    if (value == null) {
      _values.remove(key);
    } else {
      _values[key] = value;
    }
  }
}

/// A JSON file on disk (desktop and server apps).
class FileStorage implements IdentityStorage {
  FileStorage(this.file);
  final File file;

  Map<String, dynamic> _load() {
    if (!file.existsSync()) return {};
    try {
      return (jsonDecode(file.readAsStringSync()) as Map).cast<String, dynamic>();
    } on FormatException {
      return {};
    }
  }

  @override
  String? getString(String key) => _load()[key] as String?;

  @override
  void setString(String key, String? value) {
    final values = _load();
    if (value == null) {
      values.remove(key);
    } else {
      values[key] = value;
    }
    file.parent.createSync(recursive: true);
    file.writeAsStringSync(jsonEncode(values));
  }
}
