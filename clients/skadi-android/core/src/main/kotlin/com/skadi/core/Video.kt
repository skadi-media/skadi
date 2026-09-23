package com.skadi.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/**
 * Video library models (SKADI-T-0575).
 *
 * The app was audiobook-shaped end to end — `Book`, `BookFile`, `listBooks` —
 * so adding video is really making the client multi-domain. These mirror
 * [Book]/[BookFile]'s conventions on purpose: a subset of the server's fields
 * with `ignoreUnknownKeys`, so the server can grow fields without breaking the
 * phone.
 *
 * **Direct play only.** skadi has no encoder, so the device decodes what is on
 * disk. Measured over the real library that is ~98 % viable (HEVC/H.264 with
 * AAC/AC3/E-AC3, all of which ExoPlayer handles); the residue is DTS, which will
 * fail with no audio rather than gracefully. See SKADI-T-0574.
 */
@Serializable
data class MovieEdition(
    val id: String,
    val kind: String = "",
    /** Opaque status envelope, same convention as [BookFile.status]: the shape
     *  differs per variant and the client only needs the discriminant. */
    val status: JsonElement? = null,
)

/** TMDB franchise grouping, e.g. "The Taken Collection" (SKADI-T-0581). */
@Serializable
data class MovieCollection(
    val tmdb_id: Long,
    val name: String,
)

@Serializable
data class Movie(
    val id: String,
    val title: String,
    val external_ids: ExternalIds? = null,
    val year: Int? = null,
    val overview: String? = null,
    val poster_url: String? = null,
    val backdrop_url: String? = null,
    val runtime_minutes: Int? = null,
    val monitored: Boolean = true,
    /** TMDB genre names (SKADI-T-0605); empty until the server has refreshed metadata. */
    val genres: List<String> = emptyList(),
    /** US MPAA certification, e.g. "PG-13" (SKADI-T-0610); null when unrated. */
    val content_rating: String? = null,
    val collection: MovieCollection? = null,
    val editions: List<MovieEdition> = emptyList(),
) {
    /**
     * The edition that actually has bytes to play, if any.
     *
     * Mirrors [Book.importedFileId]: a movie with no imported edition is in the
     * library but not on disk, and offering Play for it would fail at the player
     * rather than at the button.
     */
    val playableEditionId: String?
        get() = editions.firstOrNull { it.statusName == "Imported" }?.id

    /** Title with year, the way a library list wants it. */
    val displayTitle: String
        get() = year?.let { "$title ($it)" } ?: title
}

/**
 * The status variant's name, or `null`.
 *
 * The server serialises an externally-tagged enum, so `{"Imported": {...}}` —
 * the discriminant is the single key. A unit variant serialises as a bare
 * string instead, which is why both shapes are handled: assuming the object form
 * would silently treat every `"Missing"` as unknown.
 */
val MovieEdition.statusName: String?
    get() = when (val s = status) {
        null -> null
        is kotlinx.serialization.json.JsonPrimitive -> s.contentOrNull
        is JsonObject -> s.keys.firstOrNull()
        else -> (s as? JsonObject)?.keys?.firstOrNull()
    }

private val kotlinx.serialization.json.JsonPrimitive.contentOrNull: String?
    get() = if (isString) content else null

/** One season's summary, for the season picker. */
@Serializable
data class Season(
    val number: Int,
    val episode_count: Int = 0,
    val monitored: Boolean = true,
)

@Serializable
data class Episode(
    val id: String,
    val season: Int,
    val number: Int,
    val title: String? = null,
    val air_date: String? = null,
    val monitored: Boolean = true,
    val status: JsonElement? = null,
) {
    /**
     * Whether this episode has bytes to play.
     *
     * The list view asks `?view=summary`, where the server sends the status
     * **variant name** as a bare string (SKADI-T-0494); the detail view gets the
     * full externally-tagged object. Both shapes must be read or the whole
     * library looks unplayable in one view and fine in the other.
     */
    val playable: Boolean
        get() = statusName == "Imported"

    /** `S01E03`, zero-padded — the form people scan a list by. */
    val code: String
        get() = "S%02dE%02d".format(season, number)
}

val Episode.statusName: String?
    get() = when (val s = status) {
        null -> null
        is kotlinx.serialization.json.JsonPrimitive -> if (s.isString) s.content else null
        is JsonObject -> s.keys.firstOrNull()
        else -> null
    }

/** Provider ids as the daemon serialises them (`external_ids`). */
@Serializable
data class ExternalIds(
    val tmdb: Long? = null,
    val tvdb: Long? = null,
    val imdb: String? = null,
    val asin: String? = null,
)

@Serializable
data class Series(
    val id: String,
    val title: String,
    val external_ids: ExternalIds? = null,
    val year: Int? = null,
    val overview: String? = null,
    val poster_url: String? = null,
    val backdrop_url: String? = null,
    val network: String? = null,
    /** `Continuing` / `Ended`, as the metadata source reports it. */
    val status: String? = null,
    val runtime_minutes: Int? = null,
    val monitored: Boolean = true,
    /** TMDB genre names (SKADI-T-0605); empty until the server has refreshed metadata. */
    val genres: List<String> = emptyList(),
    /** US TV parental guideline, e.g. "TV-14" (SKADI-T-0610); null when unrated. */
    val content_rating: String? = null,
    val seasons: List<Season> = emptyList(),
    val episodes: List<Episode> = emptyList(),
) {
    /** Episodes that can actually be played, newest season first. */
    val playableCount: Int
        get() = episodes.count { it.playable }

    /**
     * Regular seasons that have at least one playable episode.
     *
     * Specials (season 0) are included only when they have something to play —
     * a Specials row that opens to an empty list is worse than no row, and most
     * series carry one whether or not anything was ever acquired.
     */
    val seasonsWithContent: List<Int>
        get() = episodes.filter { it.playable }.map { it.season }.distinct().sorted()
}
