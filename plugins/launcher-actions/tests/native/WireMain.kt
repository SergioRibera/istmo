// Prints `LauncherActionLink.encode` for the cases of
// `tests/deep_link_wire.rs`, one `label hex` line each.
import dev.istmo.plugins.launcher_actions.LauncherActionLink

private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

private fun case(label: String, uri: String, id: String?, ms: Long?) {
    val source = id?.let { LauncherActionLink.SOURCE_PREFIX + it }
    println("$label ${hex(LauncherActionLink.encode(uri, source, ms))}")
}

fun main() {
    case("full", "notes://compose", "compose", 1_700_000_000_123L)
    case("bare", "notes://x", null, null)
    case("small-ms", "com.example.notes:/a?b=c", "a.b-c_9", 250L)
    case("mid-ms", "notes://é", "x", 65_536L)
}
