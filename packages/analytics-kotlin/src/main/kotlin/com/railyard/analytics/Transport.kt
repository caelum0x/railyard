package com.railyard.analytics

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/** One HTTP answer. */
data class HttpResult(val status: Int, val body: String)

/** How the SDK reaches the analytics service; swap it in tests. */
fun interface Transport {
    /** POST a JSON body; throws [IOException] on network failure. */
    @Throws(IOException::class)
    fun post(url: String, json: String): HttpResult
}

/** `HttpURLConnection` transport: works on Android and any JVM. */
class UrlConnectionTransport(private val timeoutMs: Int = 15_000) : Transport {
    override fun post(url: String, json: String): HttpResult {
        val conn = URL(url).openConnection() as HttpURLConnection
        try {
            conn.requestMethod = "POST"
            conn.connectTimeout = timeoutMs
            conn.readTimeout = timeoutMs
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            conn.outputStream.use { it.write(json.toByteArray(Charsets.UTF_8)) }
            val status = conn.responseCode
            val stream = if (status in 200..299) conn.inputStream else conn.errorStream
            val body = stream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() } ?: ""
            return HttpResult(status, body)
        } finally {
            conn.disconnect()
        }
    }
}

/** Where the anonymous and identified ids persist (Android: wrap SharedPreferences). */
interface IdentityStorage {
    fun getString(key: String): String?
    fun putString(key: String, value: String?)
}

/** In-memory storage (tests, or apps that must not persist ids). */
class MemoryStorage : IdentityStorage {
    private val values = java.util.concurrent.ConcurrentHashMap<String, String>()
    override fun getString(key: String): String? = values[key]
    override fun putString(key: String, value: String?) {
        if (value == null) values.remove(key) else values[key] = value
    }
}

/** A properties file on disk (JVM apps). */
class FileStorage(private val file: java.io.File) : IdentityStorage {
    private val lock = Any()
    private fun load(): java.util.Properties = java.util.Properties().also { p ->
        if (file.exists()) file.inputStream().use { p.load(it) }
    }
    override fun getString(key: String): String? = synchronized(lock) { load().getProperty(key) }
    override fun putString(key: String, value: String?) = synchronized(lock) {
        val p = load()
        if (value == null) p.remove(key) else p.setProperty(key, value)
        file.parentFile?.mkdirs()
        file.outputStream().use { p.store(it, "railyard analytics") }
    }
}
