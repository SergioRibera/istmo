package dev.istmo.gradle

import com.android.build.api.variant.Variant
import org.gradle.api.GradleException
import org.w3c.dom.Element
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import javax.xml.transform.OutputKeys
import javax.xml.transform.TransformerFactory
import javax.xml.transform.dom.DOMSource
import javax.xml.transform.stream.StreamResult

/**
 * One istmo plugin crate linked into the app.
 *
 * @property crateName plugin id (or crate name) used in messages.
 * @property nativeDir `native/android/` — Kotlin sources.
 * @property manifest `native/android/AndroidManifest.xml` when present:
 *   merged into every variant's manifest (providers, receivers,
 *   intent filters, permissions, `<queries>`).
 * @property resDir `native/android/res/` when present: added as a
 *   resource source directory (e.g. `xml/file_paths.xml`).
 * @property minAndroidApi `[min_versions] android` from `istmo.toml`.
 */
data class LinkedPlugin(
    val crateName: String,
    val nativeDir: File,
    val manifest: File?,
    val resDir: File?,
    val minAndroidApi: Int?,
)

/** Linking steps shared by `dev.istmo.app` and `dev.istmo.plugin-loader`. */
internal object PluginLinking {
    private const val XMLNS_NS = "http://www.w3.org/2000/xmlns/"
    private const val ANDROID_NS = "http://schemas.android.com/apk/res/android"
    private const val TOOLS_NS = "http://schemas.android.com/tools"

    /**
     * Merge every plugin manifest into [variant] (AGP 8.3+ only; older AGPs
     * use [writeCombinedManifest] as a build-type overlay) and enforce each
     * plugin's minimum API level.
     */
    fun wireVariant(variant: Variant, plugins: List<LinkedPlugin>, variantManifests: Boolean, tag: String) {
        if (plugins.isEmpty()) return
        checkMinSdk(variant, plugins, tag)
        if (!variantManifests) return
        for (manifest in plugins.mapNotNull { it.manifest }) {
            variant.sources.manifests.addStaticManifestFile(manifest.absolutePath)
        }
    }

    /**
     * Concatenate the children of every `<manifest>` (and of every
     * `<application>`) in [sources] into a single manifest at [dest].
     */
    fun writeCombinedManifest(sources: List<File>, dest: File) {
        val factory = DocumentBuilderFactory.newInstance().apply { isNamespaceAware = true }
        val builder = factory.newDocumentBuilder()
        val out = builder.newDocument()
        val root = out.createElement("manifest")
        root.setAttributeNS(XMLNS_NS, "xmlns:android", ANDROID_NS)
        root.setAttributeNS(XMLNS_NS, "xmlns:tools", TOOLS_NS)
        out.appendChild(root)
        val application = out.createElement("application")
        for (source in sources) {
            val doc = builder.parse(source)
            val children = doc.documentElement.childNodes
            for (i in 0 until children.length) {
                val child = children.item(i) as? Element ?: continue
                if (child.tagName == "application") {
                    val appChildren = child.childNodes
                    for (j in 0 until appChildren.length) {
                        val appChild = appChildren.item(j) as? Element ?: continue
                        application.appendChild(out.importNode(appChild, true))
                    }
                } else {
                    root.appendChild(out.importNode(child, true))
                }
            }
        }
        root.appendChild(application)
        dest.parentFile.mkdirs()
        val transformer = TransformerFactory.newInstance().newTransformer().apply {
            setOutputProperty(OutputKeys.INDENT, "yes")
        }
        dest.outputStream().use { transformer.transform(DOMSource(out), StreamResult(it)) }
    }

    private fun checkMinSdk(variant: Variant, plugins: List<LinkedPlugin>, tag: String) {
        val minSdk = try {
            variant.minSdk.apiLevel
        } catch (_: LinkageError) {
            // `Variant.minSdk` replaced `minSdkVersion` in AGP 8.1.
            @Suppress("DEPRECATION")
            variant.minSdkVersion.apiLevel
        }
        val violations = plugins.filter { (it.minAndroidApi ?: 0) > minSdk }
        if (violations.isEmpty()) return
        val report = violations.joinToString("\n") {
            "  - ${it.crateName} requires minSdk >= ${it.minAndroidApi}"
        }
        throw GradleException(
            "$tag: variant '${variant.name}' has minSdk $minSdk, but:\n$report\n" +
                "Raise minSdk (or `[min_versions] android` in istmo.toml) or drop the plugin.",
        )
    }
}
