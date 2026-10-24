package com.skadi.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * New imports (SKADI-T-0746, initiative COLLIERY-I-0653): media new to the
 * library, as `GET /new-imports` gives it. The server filters each row by the
 * member's policy, so the phone shows what it is given.
 */
@Serializable
data class NewImport(
    val id: String,
    /** RFC 3339 time it landed. */
    val at: String,
    /** `movie`, `series` or `audiobook`. */
    val kind: String,
    val quality: String? = null,
    /** The file replaced one the library already held. */
    val upgrade: Boolean = false,
    /** The movie, series or book to open. */
    @SerialName("item_id") val itemId: String,
    val title: String,
    /** `S01E05` for an episode. */
    val episode: String? = null,
    @SerialName("episode_title") val episodeTitle: String? = null,
    val year: Int? = null,
    @SerialName("poster_url") val posterUrl: String? = null,
)

/** One page of `GET /new-imports`. */
@Serializable
data class NewImportsPage(
    val items: List<NewImport> = emptyList(),
    /** Pass back for the next page; `null` at the end. */
    @SerialName("next_cursor") val nextCursor: String? = null,
)

@Serializable
internal data class NewImportsCount(val count: Int = 0)

/** The filters of the New tab: (query value, label). `null` is every kind. */
val NEW_IMPORT_KINDS: List<Pair<String?, String>> = listOf(
    null to "All",
    "movie" to "Movies",
    "series" to "TV",
    "audiobook" to "Books",
)

/** The path of a page: `/new-imports?limit=…&kind=…&cursor=…`. */
fun newImportsPath(kind: String?, cursor: String?, limit: Int): String = buildString {
    append("/new-imports?limit=").append(limit)
    kind?.let { append("&kind=").append(java.net.URLEncoder.encode(it, "UTF-8")) }
    cursor?.let { append("&cursor=").append(java.net.URLEncoder.encode(it, "UTF-8")) }
}

/** The season of an `S01E05` label, or `null`. */
fun seasonOf(episode: String?): Int? {
    val rest = episode?.removePrefix("S")?.takeIf { it != episode } ?: return null
    val end = rest.indexOf('E').takeIf { it > 0 } ?: return null
    return rest.substring(0, end).toIntOrNull()
}

/** The line under the title: `S01E05 · Pilot` for an episode, else the year. */
fun NewImport.subtitle(): String = when {
    episode != null && !episodeTitle.isNullOrEmpty() -> "$episode · $episodeTitle"
    episode != null -> episode
    else -> year?.toString() ?: ""
}

/** `FILM`, `TV` or `BOOK`. */
fun NewImport.kindTag(): String = when (kind) {
    "series" -> "TV"
    "audiobook" -> "BOOK"
    else -> "FILM"
}

/** The badge text: the count, capped so the tab stays narrow. `null` for none. */
fun newImportsBadge(count: Int): String? = when {
    count <= 0 -> null
    count > 99 -> "99+"
    else -> count.toString()
}

/** A refreshed first page in front of what is shown, newest first, no row twice. */
fun mergeNewer(shown: List<NewImport>, fresh: List<NewImport>): List<NewImport> {
    val have = shown.mapTo(HashSet()) { it.id }
    return fresh.filter { it.id !in have } + shown
}
