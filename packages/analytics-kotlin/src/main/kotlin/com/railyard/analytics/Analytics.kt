package com.railyard.analytics

import java.io.IOException
import java.time.Instant
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit

/** Configuration. [site] is the app's public site key. */
data class AnalyticsConfig(
    val site: String,
    val host: String = DEFAULT_HOST,
    /** Send when this many events are queued. */
    val flushAt: Int = 20,
    /** Also send on this interval; 0 disables the timer. */
    val flushIntervalSeconds: Long = 10,
    /** Oldest events are dropped beyond this many queued. */
    val maxQueue: Int = 1000,
) {
    companion object {
        const val DEFAULT_HOST = "https://pa.railyard.ereboslab.com"
    }
}

/** A feature flag as the service evaluated it. */
data class FeatureFlag(val key: String, val enabled: Boolean, val variant: String?)

/**
 * Native client for Railyard product analytics (PostHog-compatible wire
 * protocol) for Android and the JVM: capture, screens, identify, alias,
 * groups and feature flags. Events queue in memory and go out in batches on
 * a background thread. Thread-safe.
 */
class Analytics @JvmOverloads constructor(
    private val config: AnalyticsConfig,
    private val transport: Transport = UrlConnectionTransport(),
    private val storage: IdentityStorage = MemoryStorage(),
    private val clock: () -> Instant = { Instant.now() },
) : AutoCloseable {
    companion object {
        const val LIB_NAME = "railyard-analytics-kotlin"
        const val LIB_VERSION = "0.1.0"
        internal const val ANON_KEY = "railyard.analytics.anonymous_id"
        internal const val DISTINCT_KEY = "railyard.analytics.distinct_id"
        private const val BATCH_SIZE = 100
    }

    private val lock = Any()
    private val flushLock = Any()
    private val queue = ArrayDeque<Map<String, Any?>>()
    private val superProperties = LinkedHashMap<String, Any?>()
    private val groups = LinkedHashMap<String, String>()
    @Volatile private var flags: Map<String, FeatureFlag> = emptyMap()
    private val executor: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "railyard-analytics").apply { isDaemon = true }
    }
    private val base: String
    private val flushAt: Int = config.flushAt.coerceAtLeast(1)
    private val maxQueue: Int = config.maxQueue.coerceAtLeast(1)

    init {
        require(config.site.isNotBlank() && config.site.length <= 200) { "site must be a site key of 1 to 200 characters" }
        require(config.host.startsWith("https://") || config.host.startsWith("http://")) { "host must be an http(s) URL" }
        base = config.host.trimEnd('/')
        if (config.flushIntervalSeconds > 0) {
            executor.scheduleWithFixedDelay({ runCatching { flush() } }, config.flushIntervalSeconds, config.flushIntervalSeconds, TimeUnit.SECONDS)
        }
    }

    /** The identified id, else a persisted anonymous id. */
    val distinctId: String
        get() = synchronized(lock) { storage.getString(DISTINCT_KEY) ?: anonymousId() }

    private fun anonymousId(): String = storage.getString(ANON_KEY) ?: UUID.randomUUID().toString().also { storage.putString(ANON_KEY, it) }

    /** Properties sent with every event. */
    fun register(properties: Map<String, Any?>) = synchronized(lock) { superProperties.putAll(properties) }

    fun capture(event: String, properties: Map<String, Any?> = emptyMap()) {
        if (event.isEmpty() || event.length > 200) return
        val size = synchronized(lock) {
            val props = LinkedHashMap<String, Any?>()
            props["\$lib"] = LIB_NAME
            props["\$lib_version"] = LIB_VERSION
            props["\$os"] = System.getProperty("os.name")
            props["\$os_version"] = System.getProperty("os.version")
            props.putAll(superProperties)
            if (groups.isNotEmpty()) props["\$groups"] = LinkedHashMap(groups)
            props.putAll(properties)
            queue.addLast(
                mapOf(
                    "event" to event,
                    "distinct_id" to (storage.getString(DISTINCT_KEY) ?: anonymousId()),
                    "properties" to props,
                    "timestamp" to clock().toString(),
                    "uuid" to UUID.randomUUID().toString(),
                ),
            )
            while (queue.size > maxQueue) queue.removeFirst()
            queue.size
        }
        if (size >= flushAt) executor.execute { runCatching { flush() } }
    }

    /** A screen view (`$screen`). */
    fun screen(name: String, properties: Map<String, Any?> = emptyMap()) = capture("\$screen", properties + ("\$screen_name" to name))

    /** Tie the current anonymous user to [distinctId] and set person properties. */
    @JvmOverloads
    fun identify(distinctId: String, set: Map<String, Any?> = emptyMap(), setOnce: Map<String, Any?> = emptyMap()) {
        if (distinctId.isEmpty()) return
        val previous = synchronized(lock) {
            val prev = storage.getString(DISTINCT_KEY) ?: anonymousId()
            storage.putString(DISTINCT_KEY, distinctId)
            prev
        }
        val props = LinkedHashMap<String, Any?>()
        props["\$anon_distinct_id"] = previous
        if (set.isNotEmpty()) props["\$set"] = set
        if (setOnce.isNotEmpty()) props["\$set_once"] = setOnce
        capture("\$identify", props)
    }

    /** Another id for the current user. */
    fun alias(alias: String) = capture("\$create_alias", mapOf("alias" to alias))

    /** Associate later events with a group and set its properties. */
    @JvmOverloads
    fun group(type: String, key: String, properties: Map<String, Any?> = emptyMap()) {
        synchronized(lock) { groups[type] = key }
        val props = linkedMapOf<String, Any?>("\$group_type" to type, "\$group_key" to key)
        if (properties.isNotEmpty()) props["\$group_set"] = properties
        capture("\$groupidentify", props)
    }

    /** Forget the user (log out): new anonymous id, no groups, no flags. */
    fun reset() = synchronized(lock) {
        storage.putString(DISTINCT_KEY, null)
        storage.putString(ANON_KEY, null)
        groups.clear()
        flags = emptyMap()
    }

    val queuedCount: Int get() = synchronized(lock) { queue.size }

    /**
     * Send everything queued (blocking). Network errors, 429 and 5xx keep the
     * events for the next flush; other 4xx answers drop them.
     */
    fun flush(): Boolean = synchronized(flushLock) {
        while (true) {
            val batch = synchronized(lock) { queue.take(BATCH_SIZE) }
            if (batch.isEmpty()) return true
            val body = Json.write(mapOf("api_key" to config.site, "batch" to batch, "sent_at" to clock().toString()))
            val status = try {
                transport.post("$base/batch/", body).status
            } catch (e: IOException) {
                return false
            }
            when {
                status in 200..299 -> synchronized(lock) { repeat(minOf(batch.size, queue.size)) { queue.removeFirst() } }
                status == 429 || status >= 500 -> return false
                else -> {
                    synchronized(lock) { repeat(minOf(batch.size, queue.size)) { queue.removeFirst() } }
                    return false
                }
            }
        }
        @Suppress("UNREACHABLE_CODE")
        return true
    }

    /** Fetch the flags for the current user (blocking). */
    @JvmOverloads
    fun reloadFeatureFlags(personProperties: Map<String, Any?> = emptyMap()): Boolean {
        val body = LinkedHashMap<String, Any?>()
        body["api_key"] = config.site
        body["distinct_id"] = distinctId
        if (personProperties.isNotEmpty()) body["person_properties"] = personProperties
        synchronized(lock) { if (groups.isNotEmpty()) body["groups"] = LinkedHashMap(groups) }
        val result = try {
            transport.post("$base/flags/", Json.write(body))
        } catch (e: IOException) {
            return false
        }
        if (result.status !in 200..299) return false
        val parsed = runCatching { Json.parse(result.body) }.getOrNull() as? Map<*, *> ?: return false
        val list = parsed["flags"] as? Map<*, *> ?: return false
        flags = list.entries.mapNotNull { (k, v) ->
            val f = v as? Map<*, *> ?: return@mapNotNull null
            val key = k.toString()
            key to FeatureFlag(key, f["enabled"] == true, f["variant"] as? String)
        }.toMap()
        return true
    }

    fun isFeatureEnabled(key: String): Boolean = flags[key]?.enabled ?: false

    /** The variant of a multivariate flag, `"true"` for an enabled boolean flag, else null. */
    fun featureFlag(key: String): String? = flags[key]?.takeIf { it.enabled }?.let { it.variant ?: "true" }

    /** Flush and stop the background thread. */
    override fun close() {
        runCatching { flush() }
        executor.shutdown()
    }
}
