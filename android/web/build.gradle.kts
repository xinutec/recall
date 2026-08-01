plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "org.recall.web"
    compileSdk = 36
    // Pin to the build-tools the nix SDK provides (same as :app — AGP would
    // otherwise pick a version that isn't in the read-only SDK).
    buildToolsVersion = "36.0.0"

    defaultConfig {
        applicationId = "org.recall.web"
        // minSdk 26 (Android 8) — the system WebView is Chromium on any such device,
        // so the Angular UI renders exactly as it does in Chrome.
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1"
    }

    buildTypes {
        // Sideloaded build — no shrinking, signed with the debug key for simplicity.
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// Say so in a sentence rather than a stacktrace when the shell isn't beside us.
// Resolved against rootDir (android/), so this is the same path settings.gradle.kts
// includes — file() here would resolve against web/ and never match.
require(rootDir.resolve("../../ui-harness/android").isDirectory) {
    "ui-harness must be checked out beside this repo (~/Code/ui-harness)"
}

dependencies {
    // The shared WebView shell (ui-harness/android), substituted to a project by
    // settings.gradle.kts. No version, ever: it resolves by path.
    implementation("org.xinutec:shell")
    // WebView is part of the framework — the only other dependency is core-ktx for
    // the small conveniences. No Compose, no AppCompat: this app is a single WebView.
    implementation(libs.androidx.core.ktx)
}
