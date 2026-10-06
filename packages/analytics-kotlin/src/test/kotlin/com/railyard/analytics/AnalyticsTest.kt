package com.railyard.analytics

import com.sun.net.httpserver.HttpServer
import java.io.IOException
import java.net.InetSocketAddress
import java.util.concurrent.CopyOnWriteArrayList

/** Dependency-free test runner: `main` runs every check and exits non-zero on a failure. */
private val failures = mutableListOf<String>()
private var passed = 0

private fun check(name: String, block: () -> Unit) {
    try {
        block()
        passed++
        println("ok   $name")
    } catch (e: Throwable) {
        failures += "$name: ${e.message}"
        println("FAIL $name: ${e.message}")
    }
}

private fun expect(cond: Boolean, message: () -> String) {
    if (!cond) throw AssertionError(message())
}

private class FakeTransport : Transport {
    val requests = CopyOnWriteArrayList<Pair<String, Map<*, *>>>()
    val statuses = ArrayDeque<Int>()
    var flagsBody = "{}"
    var offline = false

    override fun post(url: String, json: String): HttpResult {
        if (offline) throw IOException("offline")
        requests += url to (Json.parse(json) as Map<*, *>)
        if (url.endsWith("/flags/")) return HttpResult(200, flagsBody)
        return HttpResult(statuses.removeFirstOrNull() ?: 200, "{\"status\":1}")
    }
}

private fun client(t: Transport, storage: IdentityStorage = MemoryStorage(), flushAt: Int = 100, maxQueue: Int = 1000) =
    Analytics(AnalyticsConfig(site = "site_123", host = "https://pa.test/", flushAt = flushAt, flushIntervalSeconds = 0, maxQueue = maxQueue), t, storage)

@Suppress("UNCHECKED_CAST")
private fun batchOf(req: Pair<String, Map<*, *>>) = req.second["batch"] as List<Map<String, Any?>>

fun main() {
    check("json round-trips nested values and escapes") {
        val v = mapOf("a" to listOf(1L, 2.5, true, null), "s" to "q\"\\\n\u0001é")
        expect(Json.parse(Json.write(v)) == v) { Json.write(v) }
        expect(runCatching { Json.parse("{\"a\":") }.isFailure) { "truncated JSON must fail" }
    }

    check("capture batches with the site key and library properties") {
        val t = FakeTransport()
        val a = client(t)
        a.capture("signed_up", mapOf("plan" to "pro"))
        a.screen("Home")
        expect(a.flush()) { "flush" }
        val (url, body) = t.requests.single()
        expect(url == "https://pa.test/batch/") { url }
        expect(body["api_key"] == "site_123") { "$body" }
        val batch = batchOf(t.requests[0])
        expect(batch.map { it["event"] } == listOf("signed_up", "\$screen")) { "$batch" }
        val props = batch[0]["properties"] as Map<*, *>
        expect(props["plan"] == "pro" && props["\$lib"] == "railyard-analytics-kotlin") { "$props" }
        expect((batch[1]["properties"] as Map<*, *>)["\$screen_name"] == "Home") { "screen name" }
        expect(a.queuedCount == 0) { "queue drained" }
        a.close()
    }

    check("identify links the anonymous id and persists; reset forgets") {
        val t = FakeTransport()
        val storage = MemoryStorage()
        val a = client(t, storage)
        val anon = a.distinctId
        a.identify("user_42", set = mapOf("email" to "a@b.c"))
        a.capture("clicked")
        a.flush()
        val batch = batchOf(t.requests[0])
        expect(batch[0]["event"] == "\$identify" && batch[0]["distinct_id"] == "user_42") { "$batch" }
        val props = batch[0]["properties"] as Map<*, *>
        expect(props["\$anon_distinct_id"] == anon) { "$props" }
        expect((props["\$set"] as Map<*, *>)["email"] == "a@b.c") { "$props" }
        expect(batch[1]["distinct_id"] == "user_42") { "later events use the id" }
        val b = client(t, storage)
        expect(b.distinctId == "user_42") { "persisted" }
        b.reset()
        expect(b.distinctId != "user_42" && b.distinctId != anon) { "fresh anonymous id" }
        a.close(); b.close()
    }

    check("server errors keep events, client errors drop them") {
        val t = FakeTransport()
        t.statuses.addAll(listOf(503, 200))
        val a = client(t)
        a.capture("one")
        expect(!a.flush() && a.queuedCount == 1) { "kept for retry" }
        expect(a.flush() && a.queuedCount == 0) { "sent on retry" }
        t.statuses.add(400)
        a.capture("bad")
        a.flush()
        expect(a.queuedCount == 0) { "a 400 never succeeds" }
        t.offline = true
        a.capture("offline")
        expect(!a.flush() && a.queuedCount == 1) { "kept while offline" }
        a.close()
    }

    check("flushAt sends in the background and the queue is bounded") {
        val t = FakeTransport()
        val a = client(t, flushAt = 2, maxQueue = 3)
        a.capture("a")
        a.capture("b")
        repeat(100) { if (t.requests.isEmpty()) Thread.sleep(10) }
        expect(t.requests.size == 1) { "background flush: ${t.requests.size}" }
        t.offline = true
        repeat(10) { a.capture("e$it") }
        expect(a.queuedCount <= 3) { "bounded: ${a.queuedCount}" }
        a.close()
    }

    check("feature flags and groups") {
        val t = FakeTransport()
        t.flagsBody = """{"flags":{"new-ui":{"key":"new-ui","enabled":true,"variant":"b"},"beta":{"key":"beta","enabled":true,"variant":null},"off":{"key":"off","enabled":false}}}"""
        val a = client(t)
        a.group("company", "acme")
        expect(a.reloadFeatureFlags()) { "flags loaded" }
        val req = t.requests.last()
        expect(req.first.endsWith("/flags/") && (req.second["groups"] as Map<*, *>)["company"] == "acme") { "$req" }
        expect(a.featureFlag("new-ui") == "b" && a.featureFlag("beta") == "true") { "variants" }
        expect(!a.isFeatureEnabled("off") && !a.isFeatureEnabled("nope")) { "off flags" }
        a.capture("after_group")
        a.flush()
        val last = batchOf(t.requests.last()).last()["properties"] as Map<*, *>
        expect((last["\$groups"] as Map<*, *>)["company"] == "acme") { "$last" }
        a.close()
    }

    check("bad configuration is refused") {
        expect(runCatching { Analytics(AnalyticsConfig(site = " "), FakeTransport()) }.isFailure) { "blank site" }
        expect(runCatching { Analytics(AnalyticsConfig(site = "s", host = "ftp://x"), FakeTransport()) }.isFailure) { "bad host" }
    }

    check("the HttpURLConnection transport talks to a real HTTP server") {
        val received = CopyOnWriteArrayList<String>()
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.createContext("/") { ex ->
            received += ex.requestMethod + " " + ex.requestURI.path + " " + ex.requestBody.readBytes().toString(Charsets.UTF_8)
            val reply = "{\"status\":1}".toByteArray()
            ex.sendResponseHeaders(200, reply.size.toLong())
            ex.responseBody.use { it.write(reply) }
        }
        server.start()
        try {
            val a = Analytics(AnalyticsConfig(site = "live", host = "http://127.0.0.1:${server.address.port}", flushIntervalSeconds = 0))
            a.capture("ping", mapOf("n" to 1))
            expect(a.flush()) { "flush over HTTP" }
            val r = received.single()
            expect(r.startsWith("POST /batch/ ") && r.contains("\"api_key\":\"live\"") && r.contains("\"event\":\"ping\"")) { r }
            a.close()
        } finally {
            server.stop(0)
        }
    }

    check("file storage persists ids across clients") {
        val f = java.io.File.createTempFile("railyard-analytics", ".properties").also { it.delete() }
        val a = client(FakeTransport(), FileStorage(f))
        a.identify("persisted")
        expect(client(FakeTransport(), FileStorage(f)).distinctId == "persisted") { "read back" }
        f.delete()
    }

    println("$passed passed, ${failures.size} failed")
    if (failures.isNotEmpty()) kotlin.system.exitProcess(1)
}
