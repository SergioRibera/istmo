pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    // `io.github.sergioribera.istmo` from the checked-out repo.
    includeBuild("../../../../../runtime/gradle-plugin")
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "notifications-probe"
include(":app")

// The runtime AAR from the checked-out repo.
includeBuild("../../../../../runtime/android") {
    dependencySubstitution {
        substitute(module("io.github.sergioribera:istmo-runtime"))
            .using(project(":"))
    }
}
