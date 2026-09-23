package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * Finding a server, and signing in to it (SKADI-T-0622).
 *
 * This used to acquire a credential by *scraping* one: skadi injected its API
 * token into the served `index.html` for any LAN visitor, and the app read it
 * out with a regex (SKADI-T-0341). That injection is gone — it handed every
 * device on the tailnet the operator's key — so the app now does what every
 * other client does and logs in (SKADI-I-0061).
 */
object Pairing {
    private val json = Json { ignoreUnknownKeys = true }

    /**
     * Is there a skadi at [baseUrl], and does it want a login?
     *
     * Probes `/api/v1/me` **unauthenticated**, which is the one call that
     * distinguishes the two cases that matter: an open-mode daemon has no auth
     * at all and answers as the operator, while a secured one answers 401. A
     * 401 is therefore a *successful* probe — it means we found skadi.
     */
    suspend fun probe(baseUrl: String, client: OkHttpClient = OkHttpClient()): ProbeResult =
        withContext(Dispatchers.IO) {
            val resp = runCatching {
                client.newCall(Request.Builder().url("$baseUrl/api/v1/me").build()).execute()
            }.getOrNull() ?: return@withContext ProbeResult.Unreachable
            resp.use {
                when {
                    it.code == 401 -> ProbeResult.NeedsLogin
                    it.isSuccessful -> {
                        // Only skadi answers /me with a member document.
                        val body = it.body?.string().orEmpty()
                        val ok = runCatching {
                            (json.parseToJsonElement(body) as JsonObject)["role"] != null
                        }.getOrDefault(false)
                        if (ok) ProbeResult.OpenMode else ProbeResult.NotSkadi
                    }
                    else -> ProbeResult.NotSkadi
                }
            }
        }

    /**
     * Exchange a username and password for this device's token.
     *
     * The token is **long-lived and is never refreshed**: the operator's
     * requirement is that a phone is logged into exactly once (SKADI-I-0061).
     * It stops working only when the account is revoked server-side.
     */
    suspend fun login(
        baseUrl: String,
        username: String,
        password: String,
        label: String,
        client: OkHttpClient = OkHttpClient(),
    ): LoginResult = withContext(Dispatchers.IO) {
        val payload = buildString {
            append("{\"username\":").append(quote(username))
            append(",\"password\":").append(quote(password))
            append(",\"label\":").append(quote(label)).append("}")
        }
        val req = Request.Builder()
            .url("$baseUrl/api/v1/auth/login")
            .post(payload.toRequestBody("application/json".toMediaType()))
            .build()
        val resp = runCatching { client.newCall(req).execute() }.getOrNull()
            ?: return@withContext LoginResult.Unreachable
        resp.use {
            if (it.code == 401) return@withContext LoginResult.Rejected
            if (!it.isSuccessful) return@withContext LoginResult.Unreachable
            val token = runCatching {
                (json.parseToJsonElement(it.body!!.string()) as JsonObject)["token"]
                    ?.jsonPrimitive?.content
            }.getOrNull()
            if (token.isNullOrEmpty()) LoginResult.Unreachable else LoginResult.Ok(token)
        }
    }

    /** JSON string literal, so a password containing quotes or backslashes survives. */
    private fun quote(s: String): String = Json.encodeToString(kotlinx.serialization.serializer(), s)
}

sealed interface ProbeResult {
    /** Found skadi; it wants a username and password. */
    data object NeedsLogin : ProbeResult
    /** Found skadi running with no authentication at all. */
    data object OpenMode : ProbeResult
    /** Something answered, but it is not skadi. */
    data object NotSkadi : ProbeResult
    /** Nothing answered. */
    data object Unreachable : ProbeResult
}

sealed interface LoginResult {
    data class Ok(val token: String) : LoginResult
    /** The username and password did not match. */
    data object Rejected : LoginResult
    data object Unreachable : LoginResult
}
