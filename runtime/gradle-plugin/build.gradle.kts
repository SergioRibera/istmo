plugins {
    `kotlin-dsl`
    `java-gradle-plugin`
    `maven-publish`
}

val pluginGroup: String by project
val pluginArtifact: String by project
val pluginVersion: String by project

group = pluginGroup
version = pluginVersion

// Repositories are declared centrally in `settings.gradle.kts` under
// `dependencyResolutionManagement`, so this project stays composable
// with consumer builds that enforce `FAIL_ON_PROJECT_REPOS`.

dependencies {
    // 8.3 is the first AGP exposing `Variant.sources.manifests`; the
    // plugin degrades gracefully (warning) when applied on older AGPs.
    compileOnly("com.android.tools.build:gradle:8.3.2")
    implementation("org.tomlj:tomlj:1.1.1")
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

gradlePlugin {
    plugins {
        create("istmoPluginLoader") {
            id = "dev.istmo.plugin-loader"
            implementationClass = "dev.istmo.gradle.IstmoLoaderPlugin"
            displayName = "istmo plugin loader"
            description =
                "Auto-injects Kotlin sources, manifest fragments and resources from every istmo plugin declared in the workspace Cargo.toml into the app's Android build, and enforces each plugin's minimum Android API level."
        }
    }
}

publishing {
    publications.withType<MavenPublication>().configureEach {
        pom {
            name.set("istmo-plugin-loader")
            description.set(
                "Gradle plugin that discovers `plugins/*/native/android/` directories from the enclosing Cargo workspace and adds them to the consumer app's Kotlin source set — the Android equivalent of Flutter's plugin auto-linking.",
            )
            url.set("https://github.com/SergioRibera/istmo")
            licenses {
                license {
                    name.set("MIT OR Apache-2.0")
                    url.set("https://spdx.org/licenses/MIT.html")
                }
            }
            scm {
                url.set("https://github.com/SergioRibera/istmo")
                connection.set("scm:git:https://github.com/SergioRibera/istmo.git")
                developerConnection.set("scm:git:git@github.com:SergioRibera/istmo.git")
            }
            developers {
                developer {
                    id.set("SergioRibera")
                    name.set("Sergio Ribera")
                }
            }
        }
    }

    repositories {
        maven {
            name = "GitHubPackages"
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
