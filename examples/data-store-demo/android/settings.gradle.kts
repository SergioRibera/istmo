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

rootProject.name = "data-store-demo"
include(":app")

includeBuild("../../../runtime/android") {
    dependencySubstitution {
        substitute(module("io.github.sergioribera:istmo-runtime"))
            .using(project(":"))
    }
}

