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
│   ├── build.gradle.kts     # `com.android.library` + `maven-publish`
│   ├── gradle.properties    # runtimeGroup / runtimeArtifact / runtimeVersion
│   └── src/main/java/dev/istmo/runtime/
│       ├── Bincode.kt
│       ├── IstmoRuntime.kt
│       └── PluginHandler.kt
└── ios/
    └── Sources/IstmoRuntime/     # source set consumed by the root Package.swift
        ├── Bincode.swift
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

Published artefact coordinates:

```
groupId    = dev.istmo
artifactId = istmo-runtime-android
version    = <matches the Rust workspace SemVer>
```

The registry is [GitHub Packages Maven][gh-packages], which requires an
authenticated HTTPS request even for public repositories. Add the
credentials to your app project once:

`settings.gradle.kts`

```kotlin
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        maven {
            url = uri("https://maven.pkg.github.com/SergioRibera/istmo")
            credentials {
                username = System.getenv("GITHUB_ACTOR")
                    ?: providers.gradleProperty("gpr.user").orNull
                password = System.getenv("GITHUB_TOKEN")
                    ?: providers.gradleProperty("gpr.key").orNull
            }
        }
    }
}
```

Local developers stash the pair in `~/.gradle/gradle.properties`:

```
gpr.user=<github-username>
gpr.key=<personal-access-token-with-read:packages>
```

`app/build.gradle.kts`

```kotlin
dependencies {
    implementation("dev.istmo:istmo-runtime-android:0.1.0")
}
```

### Composite build (working on trunk)

While iterating on the runtime itself, downstream apps can consume
`runtime/android/` directly via a Gradle composite build — no publish
step required. In the consumer's `settings.gradle.kts`:

```kotlin
includeBuild("../../runtime/android") {
    dependencySubstitution {
        substitute(module("dev.istmo:istmo-runtime-android"))
            .using(project(":"))
    }
}
```

The rest of the app's `build.gradle.kts` still writes
`implementation("dev.istmo:istmo-runtime-android:0.1.0")`; the
substitution swaps the resolved artefact for the local project.

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

Releases are cut through the
[`publish-runtime.yml`](../.github/workflows/publish-runtime.yml)
workflow — `workflow_dispatch` on GitHub Actions:

1. Bump the desired version through the workflow's `version` input.
2. The action creates the matching git tag (`v<version>`) and publishes
   the Android AAR + sources JAR to GitHub Packages under
   `dev.istmo:istmo-runtime-android:<version>`.
3. The same tag makes the Swift package resolvable via SPM's SemVer
   selector.

Tags are created by the workflow, not by hand, so the versioning
cadence is entirely driven by intentional releases.

## Versioning

The Kotlin artefact, the Swift package (via git tag) and the Rust
workspace crates all share a single SemVer track. When the workflow
publishes `0.2.0`, the same tag applies to every language surface —
consumers pin one number and get matching wire compatibility across
Rust ↔ Kotlin ↔ Swift.

[gh-packages]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-gradle-registry
