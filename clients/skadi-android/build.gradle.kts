// skadi-android (SKADI-I-0049): the native offline audiobook player.
// Layout mirrors ../../../squire/clients/squire-android (the proven shape).
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.kotlin.compose) apply false
}
