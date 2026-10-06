package com.railyard.analytics

/**
 * A small JSON writer and reader so the SDK has no dependencies (it runs on
 * Android and on any JVM). Values are `null`, `Boolean`, `Number`, `String`,
 * `Map<String, Any?>` and `List<Any?>`.
 */
internal object Json {
    fun write(value: Any?): String = StringBuilder().also { append(it, value) }.toString()

    private fun append(sb: StringBuilder, value: Any?) {
        when (value) {
            null -> sb.append("null")
            is Boolean -> sb.append(value)
            is Int, is Long, is Short, is Byte -> sb.append(value)
            is Number -> {
                val d = value.toDouble()
                if (d.isNaN() || d.isInfinite()) sb.append("null") else sb.append(value)
            }
            is String -> string(sb, value)
            is Map<*, *> -> {
                sb.append('{')
                var first = true
                for ((k, v) in value) {
                    if (!first) sb.append(',')
                    first = false
                    string(sb, k.toString())
                    sb.append(':')
                    append(sb, v)
                }
                sb.append('}')
            }
            is Iterable<*> -> {
                sb.append('[')
                value.forEachIndexed { i, v ->
                    if (i > 0) sb.append(',')
                    append(sb, v)
                }
                sb.append(']')
            }
            is Array<*> -> append(sb, value.toList())
            else -> string(sb, value.toString())
        }
    }

    private fun string(sb: StringBuilder, s: String) {
        sb.append('"')
        for (c in s) {
            when (c) {
                '"' -> sb.append("\\\"")
                '\\' -> sb.append("\\\\")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                '\t' -> sb.append("\\t")
                else -> if (c < ' ') sb.append(String.format("\\u%04x", c.code)) else sb.append(c)
            }
        }
        sb.append('"')
    }

    class ParseException(message: String) : Exception(message)

    fun parse(text: String): Any? = Parser(text).run {
        val v = value()
        skip()
        if (i != text.length) throw ParseException("trailing characters at $i")
        v
    }

    private class Parser(val s: String) {
        var i = 0

        fun skip() {
            while (i < s.length && s[i].isWhitespace()) i++
        }

        fun value(): Any? {
            skip()
            if (i >= s.length) throw ParseException("unexpected end")
            return when (s[i]) {
                '{' -> obj()
                '[' -> arr()
                '"' -> str()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> num()
            }
        }

        fun literal(word: String, v: Any?): Any? {
            if (!s.startsWith(word, i)) throw ParseException("bad literal at $i")
            i += word.length
            return v
        }

        fun obj(): Map<String, Any?> {
            val m = LinkedHashMap<String, Any?>()
            i++
            skip()
            if (i < s.length && s[i] == '}') { i++; return m }
            while (true) {
                skip()
                val k = str()
                skip()
                if (i >= s.length || s[i] != ':') throw ParseException("expected : at $i")
                i++
                m[k] = value()
                skip()
                if (i >= s.length) throw ParseException("unexpected end")
                when (s[i]) {
                    ',' -> i++
                    '}' -> { i++; return m }
                    else -> throw ParseException("expected , or } at $i")
                }
            }
        }

        fun arr(): List<Any?> {
            val l = ArrayList<Any?>()
            i++
            skip()
            if (i < s.length && s[i] == ']') { i++; return l }
            while (true) {
                l.add(value())
                skip()
                if (i >= s.length) throw ParseException("unexpected end")
                when (s[i]) {
                    ',' -> i++
                    ']' -> { i++; return l }
                    else -> throw ParseException("expected , or ] at $i")
                }
            }
        }

        fun str(): String {
            if (s[i] != '"') throw ParseException("expected string at $i")
            i++
            val sb = StringBuilder()
            while (i < s.length) {
                val c = s[i++]
                when (c) {
                    '"' -> return sb.toString()
                    '\\' -> {
                        if (i >= s.length) throw ParseException("bad escape")
                        when (val e = s[i++]) {
                            'n' -> sb.append('\n')
                            't' -> sb.append('\t')
                            'r' -> sb.append('\r')
                            'b' -> sb.append('\b')
                            'f' -> sb.append('\u000c')
                            'u' -> {
                                if (i + 4 > s.length) throw ParseException("bad unicode escape")
                                sb.append(s.substring(i, i + 4).toInt(16).toChar())
                                i += 4
                            }
                            else -> sb.append(e)
                        }
                    }
                    else -> sb.append(c)
                }
            }
            throw ParseException("unterminated string")
        }

        fun num(): Number {
            val start = i
            while (i < s.length && (s[i].isDigit() || s[i] in "+-.eE")) i++
            val t = s.substring(start, i)
            if (t.isEmpty()) throw ParseException("unexpected character at $start")
            return t.toLongOrNull() ?: t.toDoubleOrNull() ?: throw ParseException("bad number $t")
        }
    }
}
