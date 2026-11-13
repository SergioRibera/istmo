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
    }
}

rootProject.name = "istmo-android-demo"
include(":app")

// Kotlin runtime from the checked-out repo instead of the published AAR.
includeBuild("../../../runtime/android") {
    dependencySubstitution {
        substitute(module("io.github.sergioribera:istmo-runtime"))
            .using(project(":"))
    }
}

