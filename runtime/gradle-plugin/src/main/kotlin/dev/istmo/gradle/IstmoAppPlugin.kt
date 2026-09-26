package dev.istmo.gradle

import com.android.build.api.AndroidPluginVersion
import com.android.build.api.dsl.ApplicationExtension
import com.android.build.api.variant.ApplicationAndroidComponentsExtension
import org.gradle.api.GradleException
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.MapProperty
import org.gradle.api.provider.Property
import java.io.File

/**
 * `istmo { … }` — everything is optional; the defaults follow the
 * conventional `<crate>/android/` layout.
 */
open class IstmoAppExtension(project: Project) {
    /** Rust crate driving this app. Default: the parent of the Gradle root (`<crate>/android/..`). */
    val crateDir: Property<File> = project.objects.property(File::class.java)

    /** Explicit `cargo` executable. Default: `PATH`, then `$CARGO_HOME/bin`, then `~/.cargo/bin`. */
    val cargo: Property<String> = project.objects.property(String::class.java)

    /** Build type → cargo profile. Unlisted build types use `dev` for `debug` and `release` otherwise. */
    val profiles: MapProperty<String, String> =
        project.objects.mapProperty(String::class.java, String::class.java).convention(emptyMap())

    /** Extra arguments appended to every `cargo build` (e.g. `--features`, `--locked`). */
    val cargoArgs: ListProperty<String> =
        project.objects.listProperty(String::class.java).convention(emptyList())

    /** ABIs built when `android.defaultConfig.ndk.abiFilters` is empty. */
    val defaultAbis: ListProperty<String> =
        project.objects.listProperty(String::class.java).convention(listOf("arm64-v8a", "x86_64"))
}

/**
 * Gradle side of an istmo app — the Android twin of the generated Xcode
 * `build-rust.sh` + `istmo-plugins.yml`:
 *
 * - builds the Rust crate with cargo for every ABI and build type and
 *   packages `lib<crate>.so` (NDK linker configured automatically);
 * - links every istmo plugin the crate depends on — from the workspace,
 *   crates.io or git alike — through `android/.istmo/istmo.json`, written
 *   by the crate's `build.rs`: Kotlin sources, manifests, resources,
 *   `[[gradle]]` dependencies and `[min_versions] android` checks;
 * - applies istmo.toml `[app]`: `applicationId`, `versionName`,
 *   `versionCode`, `minSdk`, and the `${istmoLabel}` / `${istmoIcon}`
 *   manifest placeholders;
 * - registers `istmoDoctor`, which reports what the toolchain is missing.
 */
class IstmoAppPlugin : Plugin<Project> {

    override fun apply(project: Project) {
        val extension = project.extensions.create("istmo", IstmoAppExtension::class.java, project)
        var configured = false
        project.pluginManager.withPlugin("com.android.application") {
            configured = true
            configure(project, extension)
        }
        project.afterEvaluate {
            if (!configured) {
                throw GradleException("$TAG: apply it to an Android application module, after `com.android.application`.")
            }
        }
    }

    private fun configure(project: Project, extension: IstmoAppExtension) {
        val components = project.extensions.getByType(ApplicationAndroidComponentsExtension::class.java)
        val toolchain = Toolchain(
            project,
            cargoOverride = { extension.cargo.orNull },
            ndkLocator = { components.sdkComponents.ndkDirectory.get().asFile },
        )
        val androidRoot = project.rootDir
        val variantManifests = components.pluginVersion >= AndroidPluginVersion(8, 3)
        var metadata: IstmoMetadata? = null

        val doctorRequested = project.gradle.startParameter.taskNames.any { it.substringAfterLast(':') == "istmoDoctor" }
        components.finalizeDsl { android ->
            val crateDir = extension.crateDir.orNull ?: androidRoot.parentFile
            val abis = abisOf(android, extension)
            val store = MetadataStore(androidRoot, crateDir, toolchain)
            val loaded = try {
                store.load(rustTarget(abis.first()), android.defaultConfig.minSdk ?: DEFAULT_MIN_SDK)
            } catch (error: GradleException) {
                // Let `istmoDoctor` report what is broken instead of failing
                // the configuration it needs.
                if (!doctorRequested) throw error
                project.logger.warn("$TAG: ${error.message}")
                return@finalizeDsl
            }
            metadata = loaded
            applyIdentity(project, android, loaded.app, androidRoot)
            linkPlugins(project, android, loaded.plugins, variantManifests)
            addDependencies(project, loaded.gradle)
            registerCargoTasks(project, android, extension, toolchain, loaded, abis)
            project.logger.lifecycle(
                "$TAG: ${loaded.packageName} → lib${loaded.libName}.so for ${abis.joinToString()}; " +
                    "${loaded.plugins.size} plugin(s) linked",
            )
        }

        components.onVariants(components.selector().all()) { variant ->
            PluginLinking.wireVariant(variant, metadata?.plugins.orEmpty(), variantManifests, TAG)
        }

        project.tasks.register("istmoDoctor") {
            group = "istmo"
            description = "Checks the Rust toolchain and istmo metadata this build needs."
            doLast {
                val android = project.extensions.getByType(ApplicationExtension::class.java)
                val targets = abisOf(android, extension).map(::rustTarget)
                val checks = toolchain.checks(targets) + metadataCheck(androidRoot, metadata)
                val report = checks.joinToString("\n") { check ->
                    if (check.ok) "  ✓ ${check.label}" else "  ✗ ${check.label}\n      → ${check.fix}"
                }
                val summary = if (checks.all { it.ok }) "No issues found." else "${checks.count { !it.ok }} issue(s) found."
                project.logger.lifecycle("istmo doctor\n$report\n$summary")
            }
        }
    }

    private fun abisOf(android: ApplicationExtension, extension: IstmoAppExtension): List<String> =
        android.defaultConfig.ndk.abiFilters.toList().ifEmpty { extension.defaultAbis.get() }

    private fun rustTarget(abi: String): String =
        Toolchain.ABI_TO_RUST_TARGET[abi]
            ?: throw GradleException("$TAG: no Rust target for ABI '$abi' (supported: ${Toolchain.ABI_TO_RUST_TARGET.keys})")

    private fun metadataCheck(androidRoot: File, metadata: IstmoMetadata?): DoctorCheck {
        val file = File(androidRoot, IstmoMetadata.RELATIVE_PATH)
        return if (metadata != null) {
            DoctorCheck(true, "${file.path}: ${metadata.plugins.size} plugin(s), app ${metadata.app.id ?: metadata.packageName}")
        } else {
            DoctorCheck(false, "${file.path} not loaded", "make sure the crate's build.rs calls `istmo_build::emit()`")
        }
    }

    private fun applyIdentity(project: Project, android: ApplicationExtension, app: AppIdentity, androidRoot: File) {
        val config = android.defaultConfig
        fun <T : Any> override(name: String, current: T?, value: T, set: (T) -> Unit) {
            if (current != null && current != value) {
                project.logger.warn("$TAG: $name = $value from istmo.toml replaces $current set in Gradle")
            }
            set(value)
        }
        app.id?.let { id -> override("applicationId", config.applicationId, id) { config.applicationId = it } }
        override("versionName", config.versionName, app.versionName) { config.versionName = it }
        override("versionCode", config.versionCode, app.versionCode) { config.versionCode = it }
        app.minSdk?.let { min -> override("minSdk", config.minSdk, min) { config.minSdk = it } }
        config.manifestPlaceholders[LABEL_PLACEHOLDER] = app.name
        config.manifestPlaceholders[ICON_PLACEHOLDER] = app.icon ?: DEFAULT_ICON
        if (app.icon != null) {
            android.sourceSets.getByName("main").res.srcDir(File(androidRoot, GENERATED_RES_DIR))
        }
    }

    private fun linkPlugins(
        project: Project,
        android: ApplicationExtension,
        plugins: List<LinkedPlugin>,
        variantManifests: Boolean,
    ) {
        val main = android.sourceSets.getByName("main")
        for (plugin in plugins) {
            main.java.srcDir(plugin.nativeDir)
            plugin.resDir?.let { main.res.srcDir(it) }
        }
        if (variantManifests) return
        // AGP < 8.3 has no per-variant manifest sources: merge the plugin
        // manifests into one overlay used as each build type's manifest.
        val manifests = plugins.mapNotNull { it.manifest }
        if (manifests.isEmpty()) return
        val overlay = project.layout.buildDirectory.file("istmo/plugin-manifests/AndroidManifest.xml").get().asFile
        PluginLinking.writeCombinedManifest(manifests, overlay)
        for (buildType in android.buildTypes) {
            if (project.file("src/${buildType.name}/AndroidManifest.xml").isFile) {
                project.logger.warn(
                    "$TAG: build type '${buildType.name}' has its own manifest; merge ${overlay.path} " +
                        "into it by hand or upgrade to AGP 8.3+.",
                )
                continue
            }
            android.sourceSets.getByName(buildType.name).manifest.srcFile(overlay)
        }
    }

    private fun addDependencies(project: Project, dependencies: List<GradleDependency>) {
        for (dep in dependencies) {
            if (project.configurations.findByName(dep.scope) == null) {
                project.logger.warn("$TAG: no `${dep.scope}` configuration for ${dep.notation}; using `implementation`")
                project.dependencies.add("implementation", dep.notation)
            } else {
                project.dependencies.add(dep.scope, dep.notation)
            }
        }
    }

    private fun registerCargoTasks(
        project: Project,
        android: ApplicationExtension,
        extension: IstmoAppExtension,
        toolchain: Toolchain,
        metadata: IstmoMetadata,
        abis: List<String>,
    ) {
        val minSdk = android.defaultConfig.minSdk ?: DEFAULT_MIN_SDK
        val metadataFile = File(project.rootDir, IstmoMetadata.RELATIVE_PATH)
        for (buildType in android.buildTypes) {
            val buildTypeName = buildType.name
            val profile = extension.profiles.get()[buildTypeName] ?: if (buildTypeName == "debug") "dev" else "release"
            val jniRoot = project.layout.buildDirectory.dir("istmo/jniLibs/$buildTypeName")
            android.sourceSets.getByName(buildTypeName).jniLibs.srcDir(jniRoot.get().asFile)
            val tasks = abis.map { abi ->
                val target = rustTarget(abi)
                project.tasks.register("cargoBuild${buildTypeName.capitalized()}${abiTaskSuffix(abi)}", CargoBuildTask::class.java) {
                    description = "cargo build --target $target --profile $profile → jniLibs/$abi"
                    cargo.set(project.provider { toolchain.cargo?.path ?: "cargo" })
                    manifestPath.set(metadata.manifestPath.path)
                    rustTarget.set(target)
                    this.profile.set(profile)
                    extraArgs.set(extension.cargoArgs)
                    environment.set(project.provider { toolchain.cargoEnvironment(target, minSdk) })
                    outputDir.set(jniRoot.map { it.dir(abi) })
                    this.metadataFile.set(metadataFile)
                    metadataSnapshot.set(metadata.text)
                    doFirst { toolchain.requireReady(listOf(target)) }
                }
            }
            val suffix = "${buildTypeName.capitalized()}JniLibFolders"
            project.tasks.configureEach {
                if (name.startsWith("merge") && name.endsWith(suffix)) dependsOn(tasks)
            }
        }
    }

    private fun abiTaskSuffix(abi: String): String =
        abi.split('-', '_').joinToString("") { it.capitalized() }

    private fun String.capitalized(): String = replaceFirstChar { it.uppercaseChar() }

    private companion object {
        const val TAG = "dev.istmo.app"
        const val DEFAULT_MIN_SDK = 21
        const val LABEL_PLACEHOLDER = "istmoLabel"
        const val ICON_PLACEHOLDER = "istmoIcon"
        const val DEFAULT_ICON = "@android:drawable/sym_def_app_icon"
        const val GENERATED_RES_DIR = ".istmo/res"
    }
}
