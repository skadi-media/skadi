package com.skadi.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

/**
 * Manual hunting from the phone (SKADI-T-0602): the same three verbs the web
 * UI offers on a movie edition, an episode or a book file — search now, pick a
 * release, paste a link — plus the wanted backlog they act on.
 */

/** The API prefix an acquirable's hunting endpoints hang off (`…/releases`,
 *  `…/grab`, `…/grab-link`, `…/acquire`). Mirrors the web client's
 *  `AcquirablePath`, so the two stay in step. */
object Acquirable {
    fun movieEdition(movieId: String, editionId: String) = "/movies/$movieId/editions/$editionId"
    fun episode(seriesId: String, episodeId: String) = "/series/$seriesId/episodes/$episodeId"
    fun bookFile(bookId: String, fileId: String) = "/books/$bookId/files/$fileId"
}

/** One row of an interactive search. `release` is opaque: it is echoed back
 *  verbatim on grab, and only read for the title/size/seeders shown in the list. */
@Serializable
data class ReleaseCandidate(
    val release: JsonElement,
    val release_key: String = "",
    val quality: String = "",
    val age_days: Long = 0,
    val accepted: Boolean = false,
    val reason: String = "",
    val relevance: Float = 0f,
    val season_pack: Boolean = false,
) {
    private val obj: JsonObject? get() = release as? JsonObject
    private fun str(key: String): String? = obj?.get(key)?.let { (it as? kotlinx.serialization.json.JsonPrimitive)?.contentOrNull }
    private fun num(key: String): Long? = obj?.get(key)?.let { (it as? kotlinx.serialization.json.JsonPrimitive)?.longOrNull }

    val title: String get() = str("title") ?: release_key
    val sizeBytes: Long? get() = num("size")
    val seeders: Long? get() = num("seeders")
    /** The indexer's display name; the wire carries an id (UUID) on some
     *  releases, and a UUID under a title tells nobody anything. */
    val indexer: String? get() = str("indexer")?.takeUnless { UUID_LIKE.matches(it) }
}

// Not a companion: a `@Serializable` class already gets a generated one.
private val UUID_LIKE = Regex("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")

/** An unsatisfied edition/episode/file under a wanted item. For TV, `kind` is
 *  the episode code (`S01E03`); specials are `S00Exx`. */
@Serializable
data class WantedEdition(
    val id: String,
    val kind: String = "",
    val status_kind: String = "",
    val monitored: Boolean = true,
    val kind_name: String? = null,
    val quality_name: String? = null,
) {
    val isSpecial: Boolean get() = kind.startsWith("S00")
    val label: String get() = kind_name ?: kind
}

@Serializable
data class WantedItem(
    val kind: String,
    val id: String,
    val title: String,
    val year: Int? = null,
    val monitored: Boolean = true,
    val editions: List<WantedEdition> = emptyList(),
) {
    val hasSpecials: Boolean get() = kind == "series" && editions.any { it.isSpecial }
}

@Serializable
data class WantedSummary(
    val items: Int = 0,
    val editions: Int = 0,
    val by_status: Map<String, Int> = emptyMap(),
)

@Serializable
data class WantedResponse(
    val summary: WantedSummary = WantedSummary(),
    val items: List<WantedItem> = emptyList(),
)

@Serializable
internal data class GrabReq(val release: JsonElement)

@Serializable
internal data class GrabLinkReq(val link: String, val title: String? = null)

@Serializable
internal data class MonitoredReq(val monitored: Boolean)
