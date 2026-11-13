# istmo native runtimes

Canonical Kotlin and Swift sources for the platform half of the istmo
framework. Every consumer app links against one of the two artefacts
built from this directory instead of copy-pasting the same files into
its own tree.

## Layout

```
runtime/
├── android/                 # Gradle library project
│   ├── settings.gradle.kts
│   ├── build.gradle.kts     # `com.android.library` + `com.vanniktech.maven.publish`
│   ├── gradle.properties    # runtimeGroup / runtimeArtifact / runtimeVersion
│   └── src/main/java/dev/istmo/runtime/
│       ├── Bincode.kt
│       ├── IstmoActivities.kt       # IstmoGameActivity / IstmoNativeActivity
│       ├── IstmoHost.kt             # runtime start + generated registry lookup
│       ├── IstmoPluginRegistrant.kt
│       ├── IstmoRuntime.kt
│       └── PluginHandler.kt
├── gradle-plugin/           # `io.github.sergioribera.istmo` Gradle plugin
│   └── gradle.properties    # pluginGroup / pluginArtifact / pluginVersion
└── ios/
    └── Sources/IstmoRuntime/     # source set consumed by the root Package.swift
        ├── Bincode.swift
        ├── IstmoApp.swift        # IstmoApp.run(registerPlugins:entry:)
        ├── IstmoRuntime.swift
        ├── IstmoTransport.swift
        ├── PluginException.swift
        └── PluginHandler.swift
```

The Swift package manifest lives at the **repository root**
(`../Package.swift`). SPM does not support subpath references, so the
manifest hoists to the top of the repo and points the `IstmoRuntime`
target at `runtime/ios/Sources/IstmoRuntime`.

## Consuming from Android

Both Android artefacts are published to [Maven Central][central]:

| Artefact | Coordinates |
|---|---|
| Kotlin runtime (AAR + sources + Javadoc) | `io.github.sergioribera:istmo-runtime:<version>` |
| Gradle plugin `io.github.sergioribera.istmo` | `io.github.sergioribera:istmo-gradle-plugin:<version>` |

`<version>` matches the Rust workspace SemVer. Maven Central needs no
extra repository and no credentials:

`settings.gradle.kts`

```kotlin
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}
```

`app/build.gradle.kts`

```kotlin
dependencies {
    implementation("io.github.sergioribera:istmo-runtime:0.1.0")
}
```

### Composite build (working on trunk)

While iterating on the runtime itself, downstream apps can consume
`runtime/android/` directly via a Gradle composite build — no publish
step required. In the consumer's `settings.gradle.kts`:

```kotlin
includeBuild("../../runtime/android") {
    dependencySubstitution {
        substitute(module("io.github.sergioribera:istmo-runtime"))
            .using(project(":"))
    }
}
```

The rest of the app's `build.gradle.kts` still writes
`implementation("io.github.sergioribera:istmo-runtime:0.1.0")`; the
substitution swaps the resolved artefact for the local project.

### Gradle plugin

Apps apply `io.github.sergioribera.istmo` next to
`com.android.application`: it builds the Rust crate with cargo for every
ABI, links every istmo plugin the crate depends on, applies `istmo.toml`
`[app]` and registers the `istmoDoctor` task. With the `pluginManagement`
repositories above, Gradle resolves it from Maven Central:

```kotlin
// app/build.gradle.kts
plugins {
    id("com.android.application")
    id("io.github.sergioribera.istmo") version "0.1.0"
}
```

From a checkout, resolve it through a composite build instead (and drop
the `version`):

```kotlin
// settings.gradle.kts
pluginManagement {
    includeBuild("../../runtime/gradle-plugin")
}
```

See the [Native projects](https://sergioribera.rs/istmo/build-scripts/native-projects/)
guide.

## Consuming from Swift

The root `Package.swift` vends `IstmoRuntime` as a single SPM product.
Downstream apps reference it via a git URL + SemVer:

```swift
dependencies: [
    .package(url: "https://github.com/SergioRibera/istmo.git", from: "0.1.0"),
],
targets: [
    .target(
        name: "MyApp",
        dependencies: [.product(name: "IstmoRuntime", package: "istmo")]
    ),
],
```

### Local path (working on trunk)

Point the dependency at a checkout of the repo instead of a git URL:

```swift
dependencies: [
    .package(path: "../../"),
],
```

## Publishing

The [`publish-runtime.yml`](../.github/workflows/publish-runtime.yml)
workflow publishes both Android artefacts to Maven Central through the
Sonatype Central Portal:

1. `cargo-release` cuts the release and pushes a `v<SemVer>` tag; the
   tag push triggers the workflow.
2. The workflow reads the version from the tag and writes it into
   `runtimeVersion` (`android/gradle.properties`) and `pluginVersion`
   (`gradle-plugin/gradle.properties`) in its own checkout only —
   nothing is committed back, so trunk keeps its baseline versions.
3. It assembles the AAR and the plugin JAR, then runs
   `publishAndReleaseToMavenCentral` for each, publishing
   `io.github.sergioribera:istmo-runtime:<version>` and
   `io.github.sergioribera:istmo-gradle-plugin:<version>` (signed).
4. The Swift package has no publish step: SPM resolves it straight from
   the same git tag.

`workflow_dispatch` re-runs a release or publishes a hotfix: its
optional `version` input overrides the tag-derived version, and
`dry_run` assembles everything without publishing.

The workflow needs the repository secrets `MAVEN_CENTRAL_USERNAME` /
`MAVEN_CENTRAL_PASSWORD` (Central Portal user token) and
`MAVEN_SIGNING_KEY` / `MAVEN_SIGNING_PASSWORD` (ASCII-armored GPG
private key and its passphrase). The `io.github.sergioribera` namespace
must be verified on the Central Portal before the first publish.

## Versioning

The Kotlin runtime, the Gradle plugin, the Swift package (via git tag)
and the Rust workspace crates all share a single SemVer track. When the
workflow publishes `0.2.0`, the same tag applies to every language
surface — consumers pin one number and get matching wire compatibility
across Rust ↔ Kotlin ↔ Swift.

[central]: https://central.sonatype.com/artifact/io.github.sergioribera/istmo-runtime
