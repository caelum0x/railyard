import Foundation
import XCTest
@testable import RailyardAnalytics

final class FakeTransport: Transport, @unchecked Sendable {
    private let lock = NSLock()
    private var _requests: [(URL, [String: Any])] = []
    var statuses: [Int] = []
    var flagsBody: [String: Any] = [:]
    var failNetwork = false

    var requests: [(URL, [String: Any])] {
        lock.lock(); defer { lock.unlock() }
        return _requests
    }

    func post(url: URL, json: Data) async throws -> HTTPResult {
        if failNetwork { throw TransportError.network("offline") }
        let body = (try? JSONSerialization.jsonObject(with: json) as? [String: Any]) ?? [:]
        lock.lock()
        _requests.append((url, body))
        let status = statuses.isEmpty ? 200 : statuses.removeFirst()
        lock.unlock()
        if url.path.hasSuffix("/flags") || url.path.hasSuffix("/flags/") {
            return HTTPResult(status: 200, body: try JSONSerialization.data(withJSONObject: flagsBody))
        }
        return HTTPResult(status: status, body: Data("{\"status\":1}".utf8))
    }
}

final class AnalyticsTests: XCTestCase {
    func make(_ t: FakeTransport, flushAt: Int = 100, storage: IdentityStorage = MemoryStorage()) throws -> Analytics {
        try Analytics(config: AnalyticsConfig(site: "site_123", host: URL(string: "https://pa.test")!, flushAt: flushAt, flushInterval: 0), transport: t, storage: storage)
    }

    func testCaptureBatchesWithTheSiteKeyAndLibraryProperties() async throws {
        let t = FakeTransport()
        let a = try make(t)
        await a.capture("signed_up", properties: ["plan": "pro"])
        await a.screen("Home")
        let ok = await a.flush()
        XCTAssertTrue(ok)
        XCTAssertEqual(t.requests.count, 1)
        let (url, body) = t.requests[0]
        XCTAssertEqual(url.absoluteString, "https://pa.test/batch/")
        XCTAssertEqual(body["api_key"] as? String, "site_123")
        let batch = body["batch"] as! [[String: Any]]
        XCTAssertEqual(batch.map { $0["event"] as! String }, ["signed_up", "$screen"])
        let props = batch[0]["properties"] as! [String: Any]
        XCTAssertEqual(props["plan"] as? String, "pro")
        XCTAssertEqual(props["$lib"] as? String, "railyard-analytics-swift")
        XCTAssertEqual((batch[1]["properties"] as! [String: Any])["$screen_name"] as? String, "Home")
        XCTAssertNotNil(batch[0]["uuid"] as? String)
        let queued = await a.queuedCount
        XCTAssertEqual(queued, 0)
    }

    func testIdentifyLinksTheAnonymousIdAndPersists() async throws {
        let t = FakeTransport()
        let storage = MemoryStorage()
        let a = try make(t, storage: storage)
        let anon = await a.distinctId
        await a.identify("user_42", set: ["email": "a@b.c"])
        await a.capture("clicked")
        await a.flush()
        let batch = t.requests[0].1["batch"] as! [[String: Any]]
        XCTAssertEqual(batch[0]["event"] as? String, "$identify")
        XCTAssertEqual(batch[0]["distinct_id"] as? String, "user_42")
        let props = batch[0]["properties"] as! [String: Any]
        XCTAssertEqual(props["$anon_distinct_id"] as? String, anon)
        XCTAssertEqual((props["$set"] as? [String: Any])?["email"] as? String, "a@b.c")
        XCTAssertEqual(batch[1]["distinct_id"] as? String, "user_42")
        // A new client on the same storage keeps the id; reset forgets it.
        let b = try make(t, storage: storage)
        let kept = await b.distinctId
        XCTAssertEqual(kept, "user_42")
        await b.reset()
        let fresh = await b.distinctId
        XCTAssertNotEqual(fresh, "user_42")
        XCTAssertNotEqual(fresh, anon)
    }

    func testServerErrorsKeepEventsAndClientErrorsDropThem() async throws {
        let t = FakeTransport()
        t.statuses = [503, 200]
        let a = try make(t)
        await a.capture("one")
        var ok = await a.flush()
        XCTAssertFalse(ok)
        var queued = await a.queuedCount
        XCTAssertEqual(queued, 1, "kept for retry")
        ok = await a.flush()
        XCTAssertTrue(ok)
        t.statuses = [400]
        await a.capture("bad")
        _ = await a.flush()
        queued = await a.queuedCount
        XCTAssertEqual(queued, 0, "a 400 never succeeds; dropped")
        t.failNetwork = true
        await a.capture("offline")
        ok = await a.flush()
        XCTAssertFalse(ok)
        queued = await a.queuedCount
        XCTAssertEqual(queued, 1)
    }

    func testFlushAtTriggersASendAndTheQueueIsBounded() async throws {
        let t = FakeTransport()
        let a = try Analytics(config: AnalyticsConfig(site: "s", host: URL(string: "https://pa.test")!, flushAt: 2, flushInterval: 0, maxQueue: 3), transport: t, storage: MemoryStorage())
        await a.capture("a")
        await a.capture("b")
        for _ in 0..<50 where t.requests.isEmpty { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(t.requests.count, 1)
        t.failNetwork = true
        for i in 0..<10 { await a.capture("e\(i)") }
        let queued = await a.queuedCount
        XCTAssertLessThanOrEqual(queued, 3)
    }

    func testFeatureFlagsAndGroups() async throws {
        let t = FakeTransport()
        t.flagsBody = ["flags": ["new-ui": ["key": "new-ui", "enabled": true, "variant": "b"], "beta": ["key": "beta", "enabled": true], "off": ["key": "off", "enabled": false]]]
        let a = try make(t)
        await a.group("company", key: "acme")
        let loaded = await a.reloadFeatureFlags()
        XCTAssertTrue(loaded)
        let flagsReq = t.requests.last!
        XCTAssertTrue(flagsReq.0.absoluteString.hasSuffix("/flags/"))
        XCTAssertEqual((flagsReq.1["groups"] as? [String: String])?["company"], "acme")
        let variant = await a.featureFlag("new-ui")
        let beta = await a.featureFlag("beta")
        let off = await a.isFeatureEnabled("off")
        let unknown = await a.isFeatureEnabled("nope")
        XCTAssertEqual(variant, "b")
        XCTAssertEqual(beta, "true")
        XCTAssertFalse(off)
        XCTAssertFalse(unknown)
        await a.capture("after_group")
        await a.flush()
        let batch = t.requests.last!.1["batch"] as! [[String: Any]]
        let last = batch.last!["properties"] as! [String: Any]
        XCTAssertEqual((last["$groups"] as? [String: String])?["company"], "acme")
    }

    func testRejectsBadConfiguration() {
        XCTAssertThrowsError(try Analytics(config: AnalyticsConfig(site: " "), transport: FakeTransport(), storage: MemoryStorage()))
        XCTAssertThrowsError(try Analytics(config: AnalyticsConfig(site: "s", host: URL(string: "ftp://x")!), transport: FakeTransport(), storage: MemoryStorage()))
    }
}
