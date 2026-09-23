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

// Auto-provision the JDK the build's jvmToolchain(17) asks for (SKADI-T-0345), so
// the containerized build works on any base image (e.g. the Android-SDK builder
// ships JDK 21) — Gradle downloads + caches JDK 17 instead of failing on a
// toolchain mismatch. Harmless on hosts that already have JDK 17.
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.8.0"
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.PREFER_SETTINGS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "skadi-android"
include(":core")
include(":pairing")
include(":app")
