pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    // `io.github.sergioribera.istmo` from the checked-out repo — no maven publish needed
    // for local demos.
    includeBuild("../../../runtime/gradle-plugin")
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "share-demo"
include(":app")

// The `pluginManagement { includeBuild(...) }` above already registered
// this composite build for plugin resolution; the substitution below is
// needed only for the runtime AAR dependency.
includeBuild("../../../runtime/android") {
    dependencySubstitution {
        substitute(module("io.github.sergioribera:istmo-runtime"))
            .using(project(":"))
    }
}
