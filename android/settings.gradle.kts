pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
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

rootProject.name = "recall-android"
include(":app") // recall-mic: the always-on microphone streamer
include(":web") // recall web viewer: a WebView onto the Angular UI

// The shared WebView shell, resolved by path against the checkout beside this
// repo — no publishing, no version, no pin to bump (see ui-harness/android/README.md).
// Only :web consumes it; :app (recall-mic) is a native capture app, not a wrapper.
includeBuild("../../ui-harness/android") {
    dependencySubstitution {
        substitute(module("org.xinutec:shell")).using(project(":main"))
    }
}
