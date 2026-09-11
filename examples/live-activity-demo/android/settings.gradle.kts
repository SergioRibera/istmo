pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // GitHub Packages Maven registry — how downstream apps consume the
        // runtime once a `runtime-vX.Y.Z` release lands. The demo pins to
        // the same coordinate; the composite build below substitutes it
        // for the local `runtime/android` project when developing on
        // trunk, so no `gpr.user`/`gpr.key` credentials are required for
        // in-repo builds.
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

rootProject.name = "live-activity-demo"
include(":app")

// Composite build against the workspace-local `runtime/android` project.
// Swaps `dev.istmo:istmo-runtime` for the composite so consumers
// pick up runtime changes in the same `gradle build` — no `mavenLocal()`
// stopover required.
includeBuild("../../../runtime/android") {
    dependencySubstitution {
        substitute(module("dev.istmo:istmo-runtime"))
            .using(project(":"))
    }
}
