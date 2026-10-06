import Foundation

/// Configuration. `site` is the app's public site key.
public struct AnalyticsConfig: Sendable {
    public static let defaultHost = URL(string: "https://pa.railyard.ereboslab.com")!

    public var site: String
    public var host: URL
    /// Send when this many events are queued.
    public var flushAt: Int
    /// Also send on this interval (seconds); 0 disables the timer.
    public var flushInterval: TimeInterval
    /// Oldest events are dropped beyond this many queued.
    public var maxQueue: Int

    public init(site: String, host: URL = AnalyticsConfig.defaultHost, flushAt: Int = 20, flushInterval: TimeInterval = 10, maxQueue: Int = 1000) {
        self.site = site
        self.host = host
        self.flushAt = max(1, flushAt)
        self.flushInterval = max(0, flushInterval)
        self.maxQueue = max(1, maxQueue)
    }
}

public enum AnalyticsError: Error, Equatable {
    case invalidSite
    case invalidHost
}

/// A feature flag as the service evaluated it.
public struct FeatureFlag: Equatable, Sendable {
    public let key: String
    public let enabled: Bool
    public let variant: String?
}

/// Native client for Railyard product analytics (PostHog-compatible wire
/// protocol): capture, screens, identify, alias, groups, feature flags.
/// Events queue in memory and go out in batches.
public actor Analytics {
    public static let libName = "railyard-analytics-swift"
    public static let libVersion = "0.1.0"

    private let config: AnalyticsConfig
    private let transport: Transport
    private let storage: IdentityStorage
    private let now: @Sendable () -> Date
    private var queue: [[String: Any]] = []
    private var flags: [String: FeatureFlag] = [:]
    private var superProperties: [String: Any] = [:]
    private var timer: Task<Void, Never>?
    private var flushing = false

    static let anonKey = "railyard.analytics.anonymous_id"
    static let distinctKey = "railyard.analytics.distinct_id"

    public init(config: AnalyticsConfig, transport: Transport = URLSessionTransport(), storage: IdentityStorage = DefaultsStorage(), now: @escaping @Sendable () -> Date = { Date() }) throws {
        guard !config.site.trimmingCharacters(in: .whitespaces).isEmpty, config.site.count <= 200 else { throw AnalyticsError.invalidSite }
        guard let scheme = config.host.scheme, scheme == "https" || scheme == "http" else { throw AnalyticsError.invalidHost }
        self.config = config
        self.transport = transport
        self.storage = storage
        self.now = now
    }

    // MARK: identity

    /// The current distinct id (identified, else a persisted anonymous id).
    public var distinctId: String {
        if let id = storage.string(forKey: Self.distinctKey) { return id }
        return anonymousId
    }

    private var anonymousId: String {
        if let id = storage.string(forKey: Self.anonKey) { return id }
        let id = UUID().uuidString.lowercased()
        storage.set(id, forKey: Self.anonKey)
        return id
    }

    /// Properties sent with every event.
    public func register(_ properties: [String: Any]) {
        superProperties.merge(properties) { _, new in new }
    }

    // MARK: events

    public func capture(_ event: String, properties: [String: Any] = [:]) {
        guard !event.isEmpty, event.count <= 200 else { return }
        var props = baseProperties()
        props.merge(superProperties) { _, new in new }
        props.merge(properties) { _, new in new }
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        queue.append([
            "event": event,
            "distinct_id": distinctId,
            "properties": props,
            "timestamp": iso.string(from: now()),
            "uuid": UUID().uuidString.lowercased(),
        ])
        if queue.count > config.maxQueue {
            queue.removeFirst(queue.count - config.maxQueue)
        }
        startTimerIfNeeded()
        if queue.count >= config.flushAt {
            Task { await self.flush() }
        }
    }

    /// A screen view (`$screen`).
    public func screen(_ name: String, properties: [String: Any] = [:]) {
        var props = properties
        props["$screen_name"] = name
        capture("$screen", properties: props)
    }

    /// Tie the current anonymous user to `distinctId` and set person properties.
    public func identify(_ distinctId: String, set: [String: Any] = [:], setOnce: [String: Any] = [:]) {
        guard !distinctId.isEmpty else { return }
        let previous = self.distinctId
        storage.set(distinctId, forKey: Self.distinctKey)
        var props: [String: Any] = ["$anon_distinct_id": previous]
        if !set.isEmpty { props["$set"] = set }
        if !setOnce.isEmpty { props["$set_once"] = setOnce }
        capture("$identify", properties: props)
    }

    /// Another id for the current user.
    public func alias(_ alias: String) {
        capture("$create_alias", properties: ["alias": alias])
    }

    /// Associate later events with a group and set its properties.
    public func group(_ type: String, key: String, properties: [String: Any] = [:]) {
        var groups = (superProperties["$groups"] as? [String: String]) ?? [:]
        groups[type] = key
        superProperties["$groups"] = groups
        var props: [String: Any] = ["$group_type": type, "$group_key": key]
        if !properties.isEmpty { props["$group_set"] = properties }
        capture("$groupidentify", properties: props)
    }

    /// Forget the user (log out): new anonymous id, no groups, no flags.
    public func reset() {
        storage.set(nil, forKey: Self.distinctKey)
        storage.set(nil, forKey: Self.anonKey)
        superProperties.removeValue(forKey: "$groups")
        flags = [:]
    }

    /// Events waiting to be sent.
    public var queuedCount: Int { queue.count }

    /// Send everything queued. Network errors and 5xx/429 keep the events for
    /// the next flush; other 4xx answers drop them (they will never succeed).
    @discardableResult
    public func flush() async -> Bool {
        guard !flushing, !queue.isEmpty else { return queue.isEmpty }
        flushing = true
        defer { flushing = false }
        let batch = Array(queue.prefix(100))
        let iso = ISO8601DateFormatter()
        let body: [String: Any] = ["api_key": config.site, "batch": batch, "sent_at": iso.string(from: now())]
        guard let json = try? JSONSerialization.data(withJSONObject: body) else {
            queue.removeFirst(batch.count)
            return false
        }
        do {
            let result = try await transport.post(url: config.host.appendingPathComponent("batch/"), json: json)
            switch result.status {
            case 200..<300:
                queue.removeFirst(min(batch.count, queue.count))
                if !queue.isEmpty { return await flush() }
                return true
            case 429, 500...:
                return false
            default:
                queue.removeFirst(min(batch.count, queue.count))
                return false
            }
        } catch {
            return false
        }
    }

    // MARK: feature flags

    /// Fetch the flags for the current user.
    @discardableResult
    public func reloadFeatureFlags(personProperties: [String: Any] = [:]) async -> Bool {
        var body: [String: Any] = ["api_key": config.site, "distinct_id": distinctId]
        if !personProperties.isEmpty { body["person_properties"] = personProperties }
        if let groups = superProperties["$groups"] { body["groups"] = groups }
        guard let json = try? JSONSerialization.data(withJSONObject: body),
              let result = try? await transport.post(url: config.host.appendingPathComponent("flags/"), json: json),
              (200..<300).contains(result.status),
              let decoded = try? JSONSerialization.jsonObject(with: result.body) as? [String: Any],
              let list = decoded["flags"] as? [String: Any]
        else { return false }
        var next: [String: FeatureFlag] = [:]
        for (key, raw) in list {
            guard let f = raw as? [String: Any] else { continue }
            next[key] = FeatureFlag(key: key, enabled: f["enabled"] as? Bool ?? false, variant: f["variant"] as? String)
        }
        flags = next
        return true
    }

    public func isFeatureEnabled(_ key: String) -> Bool {
        flags[key]?.enabled ?? false
    }

    /// The variant of a multivariate flag, `"true"` for an enabled boolean flag, else nil.
    public func featureFlag(_ key: String) -> String? {
        guard let f = flags[key], f.enabled else { return nil }
        return f.variant ?? "true"
    }

    // MARK: internals

    private func baseProperties() -> [String: Any] {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        var props: [String: Any] = [
            "$lib": Self.libName,
            "$lib_version": Self.libVersion,
            "$os_version": "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)",
        ]
        #if os(iOS)
        props["$os"] = "iOS"
        #elseif os(macOS)
        props["$os"] = "macOS"
        #elseif os(tvOS)
        props["$os"] = "tvOS"
        #elseif os(watchOS)
        props["$os"] = "watchOS"
        #endif
        return props
    }

    private func startTimerIfNeeded() {
        guard timer == nil, config.flushInterval > 0 else { return }
        let interval = UInt64(config.flushInterval * 1_000_000_000)
        timer = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: interval)
                await self?.flush()
            }
        }
    }

    deinit {
        timer?.cancel()
    }
}
