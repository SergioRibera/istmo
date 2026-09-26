import com.vanniktech.maven.publish.AndroidSingleVariantLibrary
import com.vanniktech.maven.publish.SonatypeHost

plugins {
    id("com.android.library") version "8.2.2"
    id("org.jetbrains.kotlin.android") version "1.9.24"
    id("com.vanniktech.maven.publish") version "0.30.0"
}

val runtimeGroup: String by project
val runtimeArtifact: String by project
val runtimeVersion: String by project

android {
    namespace = "dev.istmo.runtime"
    compileSdk = 36

    defaultConfig {
        minSdk = 21
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    // `IstmoGameActivity` only: apps that use it declare games-activity
    // (and its AppCompatActivity supertype's appcompat artifact)
    // themselves, so the runtime does not force it on everyone else.
    compileOnly("androidx.games:games-activity:4.4.0")
    compileOnly("androidx.appcompat:appcompat:1.7.0")
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL, automaticRelease = true)
    signAllPublications()

    coordinates(runtimeGroup, runtimeArtifact, runtimeVersion)

    configure(AndroidSingleVariantLibrary(variant = "release", sourcesJar = true, publishJavadocJar = true))

    pom {
        name.set("istmo-runtime")
        description.set(
            "Kotlin-side runtime for the istmo framework — JNI transport pump, wire codec, plugin dispatcher registry.",
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

