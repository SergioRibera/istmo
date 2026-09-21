pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    // Pull in the istmo plugin loader (`dev.istmo.plugin-loader`) from
    // the checked-out repo — no maven publish needed for local demos.
    includeBuild("../../../runtime/gradle-plugin")
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
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

rootProject.name = "pen-demo"
include(":app")

// The `pluginManagement { includeBuild(...) }` above already registered
// this composite build for plugin resolution; the substitution below is
// needed only for the runtime AAR dependency.
includeBuild("../../../runtime/android") {
    dependencySubstitution {
        substitute(module("dev.istmo:istmo-runtime"))
            .using(project(":"))
    }
}
