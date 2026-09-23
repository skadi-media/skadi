package com.skadi.core

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** `/me` (SKADI-T-0614): the role decides which half of the app is drawn. */
class MeTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `admin is the operator and policy fields are ignored`() {
        val me = json.decodeFromString<Me>(
            """{"id":"admin","name":"Operator","role":"admin","has_pin":false,"policy":{"kinds":["movie"]}}""",
        )
        assertTrue(me.isAdmin)
        assertEquals("Operator", me.name)
    }

    @Test
    fun `a kid is not the operator`() {
        val me = json.decodeFromString<Me>("""{"id":"abc","name":"Sam","role":"kid"}""")
        assertFalse(me.isAdmin)
        assertEquals("kid", me.role)
    }

    @Test
    fun `a refusal reads as the server's sentence`() {
        assertEquals("Not allowed on this account", NotAllowedException().message)
    }
}
