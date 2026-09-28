import com.vanniktech.maven.publish.GradlePlugin
import com.vanniktech.maven.publish.JavadocJar
import com.vanniktech.maven.publish.SonatypeHost

plugins {
    `kotlin-dsl`
    `java-gradle-plugin`
    id("com.vanniktech.maven.publish") version "0.30.0"
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
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

gradlePlugin {
    plugins {
        create("istmoApp") {
            id = "io.github.sergioribera.istmo"
            implementationClass = "dev.istmo.gradle.IstmoAppPlugin"
            displayName = "istmo app"
            description =
                "Builds the app's Rust crate with cargo for every ABI, links every istmo plugin it depends on (Kotlin sources, manifests, resources, Gradle dependencies) and applies istmo.toml [app] (application id, version, minSdk, label, icon)."
        }
    }
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL, automaticRelease = true)
    signAllPublications()

    coordinates(pluginGroup, pluginArtifact, pluginVersion)

    // GradlePlugin bundles the plugin marker + main artifact and
    // generates sources and (empty) javadoc jars — required for Maven
    // Central acceptance. GradlePublishPlugin would need
    // `com.gradle.plugin-publish` applied and targets the Gradle
    // Plugin Portal instead.
    configure(GradlePlugin(javadocJar = JavadocJar.Empty(), sourcesJar = true))

    pom {
        name.set("istmo-gradle-plugin")
        description.set(
            "Gradle plugin for istmo apps (`dev.istmo.app`): builds the Rust crate with cargo for every ABI, links every istmo plugin it depends on and applies istmo.toml [app].",
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
