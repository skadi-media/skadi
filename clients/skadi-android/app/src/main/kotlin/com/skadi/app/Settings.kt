package com.skadi.app

import android.content.Context
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/** App settings (SKADI-T-0341): the paired server, its role, and the theme. */
val Context.dataStore by preferencesDataStore(name = "skadi")

object Settings {
    private val BASE_URL = stringPreferencesKey("base_url")
    private val TOKEN = stringPreferencesKey("token")
    // Who the token is (SKADI-T-0614): `admin` | `member` | `kid`, from `/me`.
    // Cached so a cold start draws the right bar before the network answers.
    private val ROLE = stringPreferencesKey("role")
    // The theme (SKADI-T-0721): a [SkadiThemeMode.key]. A choice of the
    // phone, not of the pairing, so unpairing keeps it.
    private val THEME = stringPreferencesKey("theme")

    data class Server(val baseUrl: String, val token: String)

    fun server(context: Context): Flow<Server?> =
        context.dataStore.data.map { p ->
            p[BASE_URL]?.let { Server(it, p[TOKEN].orEmpty()) }
        }

    suspend fun save(context: Context, baseUrl: String, token: String) {
        context.dataStore.edit { p ->
            p[BASE_URL] = baseUrl
            p[TOKEN] = token
            // A new pairing is a new person until /me says otherwise.
            p.remove(ROLE)
        }
    }

    /** The cached role, `null` until `/me` has answered once for this pairing. */
    fun role(context: Context): Flow<String?> = context.dataStore.data.map { p -> p[ROLE] }

    suspend fun saveRole(context: Context, role: String) {
        context.dataStore.edit { p -> p[ROLE] = role }
    }

    fun theme(context: Context): Flow<SkadiThemeMode> =
        context.dataStore.data.map { p -> SkadiThemeMode.fromKey(p[THEME]) }

    suspend fun saveTheme(context: Context, mode: SkadiThemeMode) {
        context.dataStore.edit { p -> p[THEME] = mode.key }
    }

    /** Forget the pairing. The theme stays. */
    suspend fun clear(context: Context) {
        context.dataStore.edit { p ->
            val theme = p[THEME]
            p.clear()
            theme?.let { p[THEME] = it }
        }
    }
}
