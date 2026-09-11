plugins {
    id("com.android.library") version "8.2.2"
    id("org.jetbrains.kotlin.android") version "1.9.24"
    `maven-publish`
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

    publishing {
        singleVariant("release") {
            withSourcesJar()
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}

publishing {
    publications {
        register<MavenPublication>("release") {
            groupId = runtimeGroup
            artifactId = runtimeArtifact
            version = runtimeVersion

            afterEvaluate {
                from(components["release"])
            }

            pom {
                name.set("istmo-runtime-android")
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
