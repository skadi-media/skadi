package com.skadi.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.io.File
import java.util.UUID

/** A place in a book the listener marked, with an optional note (SKADI-T-0662). */
@Serializable
data class Bookmark(
    val id: String,
    @SerialName("position_s") val positionS: Double,
    val note: String = "",
    @SerialName("created_at") val createdAt: Long = 0,
)

/**
 * A book's bookmarks, in `books/{fid}/bookmarks.json` beside its audio and
 * position (SKADI-T-0662).
 *
 * **Device-only, by decision.** Listening positions already live only on the
 * device that downloaded the book; bookmarks synced through the server while
 * positions are not would follow the listener to a device that does not know
 * where they are in the book. If positions are ever synced, bookmarks go with
 * them. Deleting the download deletes them, like the position.
 *
 * Writes go through a temp file and a rename, as `meta.json` does, so a kill
 * mid-write cannot leave a truncated list.
 */
class Bookmarks(root: File, fid: String) {
    private val file = File(root, "books/$fid/bookmarks.json")
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    private val serializer = ListSerializer(Bookmark.serializer())

    /** In book order. */
    fun list(): List<Bookmark> =
        runCatching { json.decodeFromString(serializer, file.readText()) }
            .getOrDefault(emptyList())
            .sortedBy { it.positionS }

    fun add(positionS: Double, note: String = "", now: Long = System.currentTimeMillis()): Bookmark {
        val b = Bookmark(UUID.randomUUID().toString(), positionS.coerceAtLeast(0.0), note.trim(), now)
        save(list() + b)
        return b
    }

    fun updateNote(id: String, note: String) =
        save(list().map { if (it.id == id) it.copy(note = note.trim()) else it })

    fun delete(id: String) = save(list().filterNot { it.id == id })

    private fun save(all: List<Bookmark>) {
        file.parentFile?.mkdirs()
        val tmp = File(file.parentFile, "bookmarks.json.tmp")
        tmp.writeText(json.encodeToString(serializer, all.sortedBy { it.positionS }))
        if (!tmp.renameTo(file)) {
            file.delete()
            tmp.renameTo(file)
        }
    }
}
