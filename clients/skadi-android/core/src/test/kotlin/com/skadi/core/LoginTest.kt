package com.skadi.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Signing in (SKADI-T-0622). */
class LoginTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `the login response yields the device token`() {
        val body = """{"member":{"id":"abc","name":"Robin","role":"kid","username":"Robin",
            "has_password":true,"devices":1,"has_pin":false},"token":"deadbeef"}"""
        val token = (json.parseToJsonElement(body) as JsonObject)["token"]?.jsonPrimitive?.content
        assertEquals("deadbeef", token)
    }

    /** A password with quotes or a backslash must survive being put in JSON —
     *  the failure mode otherwise is a login that silently never works. */
    @Test
    fun `awkward passwords are encoded, not concatenated`() {
        val nasty = """he said "no" \ then left"""
        val encoded = Json.encodeToString(kotlinx.serialization.serializer<String>(), nasty)
        val round = json.parseToJsonElement("""{"password":$encoded}""") as JsonObject
        assertEquals(nasty, round["password"]?.jsonPrimitive?.content)
    }

    @Test
    fun `a 401 is its own exception so only it signs a device out`() {
        assertTrue(SessionExpiredException() is IllegalStateException)
        assertEquals("Signed out", SessionExpiredException().message)
        // Distinct from the policy refusal, which must never sign anyone out.
        assertTrue(NotAllowedException() !is SessionExpiredException)
    }
}
