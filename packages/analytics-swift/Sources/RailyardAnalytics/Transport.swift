import Foundation

/// One HTTP exchange: the SDK only ever POSTs JSON.
public struct HTTPResult: Sendable {
    public let status: Int
    public let body: Data
    public init(status: Int, body: Data) {
        self.status = status
        self.body = body
    }
}

public enum TransportError: Error, Equatable {
    case network(String)
}

/// How the SDK reaches the analytics service. Swap it in tests.
public protocol Transport: Sendable {
    func post(url: URL, json: Data) async throws -> HTTPResult
}

/// `URLSession`-backed transport.
public struct URLSessionTransport: Transport {
    let session: URLSession
    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func post(url: URL, json: Data) async throws -> HTTPResult {
        var request = URLRequest(url: url, timeoutInterval: 15)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = json
        do {
            let (data, response) = try await session.data(for: request)
            return HTTPResult(status: (response as? HTTPURLResponse)?.statusCode ?? 0, body: data)
        } catch {
            throw TransportError.network(error.localizedDescription)
        }
    }
}

/// Where the anonymous and identified ids persist between launches.
public protocol IdentityStorage: Sendable {
    func string(forKey key: String) -> String?
    func set(_ value: String?, forKey key: String)
}

/// `UserDefaults`-backed storage (the default).
public final class DefaultsStorage: IdentityStorage, @unchecked Sendable {
    let defaults: UserDefaults
    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }
    public func string(forKey key: String) -> String? { defaults.string(forKey: key) }
    public func set(_ value: String?, forKey key: String) { defaults.set(value, forKey: key) }
}

/// In-memory storage (tests, or apps that must not persist ids).
public final class MemoryStorage: IdentityStorage, @unchecked Sendable {
    private var values: [String: String] = [:]
    private let lock = NSLock()
    public init() {}
    public func string(forKey key: String) -> String? {
        lock.lock(); defer { lock.unlock() }
        return values[key]
    }
    public func set(_ value: String?, forKey key: String) {
        lock.lock(); defer { lock.unlock() }
        values[key] = value
    }
}
