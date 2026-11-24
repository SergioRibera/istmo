package dev.istmo.plugins.webview

import java.net.IDN
import java.net.URI

/**
 * Kotlin twin of the Rust `rules` module: the same normalisation and
 * matching, held to `tests/rule_vectors.tsv`. URLs reaching
 * `shouldOverrideUrlLoading` are already canonical (Chromium), so the
 * work here is mostly for app-issued loads and the patterns.
 */
internal class CompiledRules(rules: NavigationRules) {
    private val defaultAction = rules.defaultAction
    private val compiled = rules.rules.map { Pattern.compile(it.pattern) to it.action }

    fun evaluate(raw: String, fileAccess: Boolean): RuleAction {
        val url = Url.parse(raw) ?: return RuleAction.Deny
        if (url.scheme in BLOCKED || (url.scheme == "file" && !fileAccess) || url.hasCredentials) {
            return RuleAction.Deny
        }
        return compiled.firstOrNull { it.first.matches(url) }?.second ?: defaultAction
    }

    private class Pattern(
        val scheme: String?,
        val host: String?,
        val subdomains: Boolean,
        val port: Int?,
        val pathPrefix: String,
    ) {
        fun matches(url: Url): Boolean {
            if (scheme != null && scheme != url.scheme) return false
            val hostOk = when {
                host == null -> true
                url.host == null -> false
                subdomains -> url.host.endsWith(".$host") && url.host.length > host.length + 1
                else -> url.host == host
            }
            if (!hostOk) return false
            val portOk = if (port != null) url.effectivePort == port else url.explicitPort == null
            return portOk && pathMatches(pathPrefix, url.path)
        }

        companion object {
            fun compile(p: UrlPattern): Pattern {
                val scheme = if (p.scheme == "*") null else p.scheme.lowercase().also {
                    require(isScheme(it)) { "bad scheme ${p.scheme}" }
                }
                var host: String? = null
                var subdomains = false
                if (p.host != "*") {
                    val domain = p.host.removePrefix("*.")
                    subdomains = domain != p.host
                    require(domain.isNotEmpty() && domain.none { it in "*/@?#" }) { "bad host ${p.host}" }
                    host = normalizeHost(domain) ?: throw IllegalArgumentException("bad host ${p.host}")
                }
                val path = when {
                    p.pathPrefix.isEmpty() -> ""
                    else -> {
                        require(p.pathPrefix.startsWith("/")) { "bad path ${p.pathPrefix}" }
                        val normalized = Url.parse("https://h${p.pathPrefix}")?.path ?: "/"
                        if (normalized.length > 1) normalized.trimEnd('/') else normalized
                    }
                }
                return Pattern(scheme, host, subdomains, p.port?.toInt(), path)
            }
        }
    }

    /** The pieces of a URL the rules look at, normalised. */
    class Url(
        val scheme: String,
        val host: String?,
        val explicitPort: Int?,
        val path: String,
        val hasCredentials: Boolean,
    ) {
        val effectivePort: Int? get() = explicitPort ?: DEFAULT_PORTS[scheme]

        companion object {
            fun parse(raw: String): Url? {
                val trimmed = raw.trim()
                val colon = trimmed.indexOf(':')
                if (colon <= 0) return null
                val scheme = trimmed.substring(0, colon).lowercase()
                if (!isScheme(scheme)) return null
                // Special schemes read `\` as `/`, like the WHATWG parser.
                val rest = trimmed.substring(colon + 1).let {
                    if (scheme in DEFAULT_PORTS) it.replace('\\', '/') else it
                }
                val uri = try {
                    URI("$scheme:$rest")
                } catch (_: Exception) {
                    return null
                }
                val credentials = uri.rawUserInfo != null
                val authorityHost = uri.rawAuthority?.substringAfterLast('@')?.let { authority ->
                    if (authority.startsWith("[")) authority.substringBefore(']') + "]"
                    else authority.substringBefore(':')
                }
                val host = authorityHost?.takeIf { it.isNotEmpty() }?.let { normalizeHost(it) ?: return null }
                val port = if (uri.port == -1 || uri.port == DEFAULT_PORTS[scheme]) null else uri.port
                val path = normalizePath(uri.rawPath ?: "", host != null || scheme in DEFAULT_PORTS)
                return Url(scheme, host, port, path, credentials)
            }
        }
    }

    companion object {
        private val BLOCKED = setOf("javascript", "intent", "data", "vbscript")
        private val DEFAULT_PORTS = mapOf("http" to 80, "https" to 443, "ws" to 80, "wss" to 443, "ftp" to 21)

        fun isScheme(s: String): Boolean =
            s.isNotEmpty() && s[0] in 'a'..'z' && s.all { it in 'a'..'z' || it in '0'..'9' || it in "+-." }

        fun pathMatches(prefix: String, path: String): Boolean {
            if (prefix.isEmpty() || prefix == "/") return true
            if (!path.startsWith(prefix)) return false
            val rest = path.substring(prefix.length)
            return rest.isEmpty() || rest.startsWith("/")
        }

        /** Lowercase, punycode, no trailing dot, WHATWG IPv4 forms. */
        fun normalizeHost(raw: String): String? {
            if (raw.startsWith("[")) return raw.lowercase()
            val ascii = try {
                IDN.toASCII(raw.trimEnd('.'), IDN.ALLOW_UNASSIGNED).lowercase()
            } catch (_: IllegalArgumentException) {
                return null
            }
            if (ascii.isEmpty()) return null
            return parseIpv4(ascii) ?: ascii
        }

        /** WHATWG IPv4 parser; `null` when `host` is not an IPv4 literal. */
        private fun parseIpv4(host: String): String? {
            val parts = host.split('.').let { if (it.size > 1 && it.last().isEmpty()) it.dropLast(1) else it }
            if (parts.isEmpty() || parts.size > 4) return null
            val numbers = parts.map { parseIpv4Number(it) ?: return null }
            if (numbers.dropLast(1).any { it > 255 }) return null
            val last = numbers.last()
            if (last >= 1L shl (8 * (5 - numbers.size))) return null
            var ipv4 = last
            numbers.dropLast(1).forEachIndexed { i, n -> ipv4 += n shl (8 * (3 - i)) }
            return (3 downTo 0).joinToString(".") { ((ipv4 shr (8 * it)) and 0xff).toString() }
        }

        private fun parseIpv4Number(part: String): Long? {
            if (part.isEmpty()) return null
            val (digits, radix) = when {
                part.startsWith("0x") || part.startsWith("0X") -> part.substring(2) to 16
                part.length > 1 && part.startsWith("0") -> part.substring(1) to 8
                else -> part to 10
            }
            if (digits.isEmpty()) return 0
            return digits.toLongOrNull(radix)
        }

        /** Resolve `.` / `..` segments. */
        private fun normalizePath(raw: String, hierarchical: Boolean): String {
            if (!hierarchical) return raw
            if (raw.isEmpty()) return "/"
            val out = ArrayList<String>()
            for (segment in raw.split('/').drop(1)) {
                when (segment.lowercase()) {
                    ".", "%2e" -> {}
                    "..", ".%2e", "%2e.", "%2e%2e" -> if (out.isNotEmpty()) out.removeAt(out.size - 1)
                    else -> out.add(segment)
                }
            }
            val trailing = raw.endsWith("/") || raw.endsWith("/.") || raw.endsWith("/..")
            return "/" + out.joinToString("/") + if (trailing && out.isNotEmpty()) "/" else ""
        }

        /** Origin of `raw` (`scheme://host[:port]`), `null` when opaque. */
        fun originOf(raw: String): String? {
            val url = Url.parse(raw) ?: return null
            if (url.host == null || url.scheme !in DEFAULT_PORTS) return null
            return url.scheme + "://" + url.host + (url.explicitPort?.let { ":$it" } ?: "")
        }
    }
}
