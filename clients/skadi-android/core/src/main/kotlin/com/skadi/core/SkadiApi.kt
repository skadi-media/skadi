package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.contentOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * Hand-rolled client for skadi's audiobook surface (SKADI-I-0049).
 * Over OkHttp + kotlinx-serialization rather than generated (skadi publishes no
 * OpenAPI spec). Originally read-only (books/chapters/audio); SKADI-I-0052 adds
 * the authed WRITES the phone needs for library management — delete a book,
 * set/clear author/series watchers, and add/mark-wanted — so the device can
 * manage the server library, not just consume it. Positions/downloads still live
 * on-device.
 */
@Serializable
data class SeriesLink(
    @SerialName("series_id") val seriesId: String = "",
    val name: String = "",
    val position: String? = null,
)

/** What `/app/manifest.json` says is on offer (SKADI-T-0579). */
@Serializable
data class AppManifest(
    val file: String,
    @SerialName("version_name") val versionName: String,
    @SerialName("version_code") val versionCode: Int,
)

@Serializable
data class BookFile(
    val id: String,
    /** Raw AcquisitionStatus JSON; the app only cares whether it's Imported. */
    val status: JsonElement,
)

@Serializable
data class Book(
    val id: String,
    val title: String,
    /** The book's own Audible ASIN — used for re-add / library actions. */
    val asin: String? = null,
    /** Where the ASIN actually lives on the wire (`external_ids.asin`). */
    val external_ids: ExternalIds? = null,
    val authors: List<String> = emptyList(),
    val narrators: List<String> = emptyList(),
    val series: SeriesLink? = null,
    @SerialName("cover_url") val coverUrl: String? = null,
    val files: List<BookFile> = emptyList(),
    /** Publisher blurb, as HTML (SKADI-T-0586). */
    val overview: String? = null,
    val year: Int? = null,
    @SerialName("runtime_minutes") val runtimeMinutes: Int? = null,
    val monitored: Boolean = true,
) {
    /** The first imported file — the downloadable audio, if any. */
    val importedFileId: String?
        get() = files.firstOrNull { f ->
            // Imported is either the object variant {"Imported":{...}} or a
            // bare string — never matched by substring (a Failed message
            // containing the word must not count).
            (f.status as? kotlinx.serialization.json.JsonObject)?.containsKey("Imported") == true ||
                ((f.status as? kotlinx.serialization.json.JsonPrimitive)?.isString == true &&
                    (f.status as kotlinx.serialization.json.JsonPrimitive).content == "Imported")
        }?.id
}

@Serializable
data class Chapter(
    val index: Int,
    val title: String,
    @SerialName("start_s") val startS: Double,
    @SerialName("end_s") val endS: Double,
)

/** An active watcher — the server auto-acquires new releases in this scope. */
@Serializable
data class Watcher(
    /** "author" | "series" | "book". */
    val scope: String,
    /** The scope's ASIN (author/series/book). */
    val key: String,
)

@Serializable
private data class AddBookReq(val asin: String, val profile: String? = null, val search: Boolean = true)

@Serializable
private data class AddMovieReq(val tmdb_id: Long, val profile: String)

@Serializable
private data class AddSeriesReq(val tvdb_id: Long, val profile: String, val monitor: String)

/** A movie hit from `GET /movies/lookup` (TMDB). */
@Serializable
data class MovieHit(
    val tmdb_id: Long,
    val title: String,
    val year: Int? = null,
    val poster_url: String? = null,
    val overview: String? = null,
)

/** A series hit from `GET /series/lookup` (TVDB). */
@Serializable
data class SeriesHit(
    val tvdb_id: Long,
    val title: String,
    val year: Int? = null,
    val poster_url: String? = null,
    val overview: String? = null,
)

/** An audiobook hit from `GET /books/search` (Audible via Audnexus). */
@Serializable
data class BookHit(
    val asin: String,
    val title: String,
    val authors: List<String> = emptyList(),
    val year: Int? = null,
    val cover_url: String? = null,
)

/** A quality profile row from `GET /settings/profiles`; only the name matters here. */
@Serializable
data class ProfileSetting(val id: String, val body: ProfileBody = ProfileBody())

@Serializable
data class ProfileBody(val name: String = "")

/** An author entity — `asin` is the watcher key for author-scope watches. */
@Serializable
data class AuthorRef(val name: String, val asin: String? = null)

/** A series completeness rollup — `seriesAsin` is the watcher key for series scope. */
@Serializable
data class SeriesRollup(
    val name: String,
    @SerialName("series_asin") val seriesAsin: String? = null,
    val total: Int = 0,
    val owned: Int = 0,
    val watched: Boolean = false,
)

/** One work in an author's or series' body of work (`/audiobooks/works`),
 *  owned or not (SKADI-T-0604). */
@Serializable
data class Work(
    val asin: String,
    val title: String,
    val authors: List<String> = emptyList(),
    @SerialName("series_name") val seriesName: String? = null,
    @SerialName("series_position") val seriesPosition: String? = null,
    @SerialName("cover_url") val coverUrl: String? = null,
    @SerialName("release_date") val releaseDate: String? = null,
    val owned: Boolean = false,
    @SerialName("book_id") val bookId: String? = null,
    val watched: Boolean = false,
)

/** `/health`: liveness plus the daemon's version (SKADI-T-0606). */
@Serializable
data class Health(val status: String = "", val version: String = "")

/** One row of `/health/checks`: `ok` / `warn` / `fail` with a sentence. */
@Serializable
data class HealthCheck(val name: String, val status: String = "", val detail: String = "")

/** `/system/status` is camelCase on the wire, unlike the rest of the API. */
@Serializable
data class DomainState(
    val name: String,
    val enabled: Boolean = true,
    @SerialName("workerFailures") val workerFailures: Long = 0,
)

/** `/system/status`: what the daemon is, how long it has been up, what it runs on. */
@Serializable
data class SystemStatus(
    val version: String = "",
    @SerialName("startTime") val startTime: String? = null,
    @SerialName("uptimeSeconds") val uptimeSeconds: Long = 0,
    val database: String? = null,
    @SerialName("libraryRoot") val libraryRoot: String? = null,
    val domains: List<DomainState> = emptyList(),
)

@Serializable
data class AuthorLookupResult(
    val asin: String,
    val name: String,
    val image: String? = null,
    val description: String? = null,
)

/** The server refused this account (HTTP 403): the member's role does not
 *  reach that route, or the title is outside their policy (SKADI-T-0614). */
class NotAllowedException : IllegalStateException("Not allowed on this account")

/** The server no longer accepts this device's token (HTTP 401): the account was
 *  revoked, or its password was changed (SKADI-T-0622). The only thing that
 *  sends a signed-in app back to the login screen. */
class SessionExpiredException : IllegalStateException("Signed out")

/** Who this token is (`GET /me`, SKADI-T-0611). */
@Serializable
data class Me(
    val id: String,
    val name: String,
    /** `admin` | `member` | `kid`. */
    val role: String,
) {
    val isAdmin: Boolean get() = role == "admin"
}

class SkadiApi(
    private val baseUrl: String,
    private val token: String?,
    private val client: OkHttpClient = OkHttpClient(),
) {
    private val json = Json { ignoreUnknownKeys = true }

    private fun request(path: String): Request {
        val b = Request.Builder().url("$baseUrl/api/v1$path")
        if (!token.isNullOrEmpty()) b.header("Authorization", "Bearer $token")
        return b.build()
    }

    /** Build an authed request for a mutating method (PUT/POST/DELETE), optional
     *  JSON body. Centralizes auth so writes can't silently go unauthenticated. */
    private fun mutate(method: String, path: String, jsonBody: String? = null): Request {
        val body = jsonBody?.toRequestBody("application/json".toMediaType())
            ?: if (method != "GET") ByteArray(0).toRequestBody() else null
        val b = Request.Builder().url("$baseUrl/api/v1$path").method(method, body)
        if (!token.isNullOrEmpty()) b.header("Authorization", "Bearer $token")
        return b.build()
    }

    /** Execute a mutating request, returning success (true) or throwing on a
     *  transport error; a non-2xx is surfaced as `false` with the code logged by
     *  the caller via [check] if it wants a hard failure. */
    private suspend fun send(req: Request): Boolean = withContext(Dispatchers.IO) {
        client.newCall(req).execute().use { it.isSuccessful }
    }

    /**
     * Is there a Skadi daemon at [baseUrl] that can actually serve us?
     *
     * Probes `/api/v1/health/ready`, NOT `/health` (SKADI-T-0462): the bare path
     * is served by the daemon's SPA fallback, so it answers 200 with HTML — and
     * so does any unrelated web server the phone happens to reach, which made
     * pairing "succeed" against the wrong host. Readiness also waits for the
     * database and providers, so a daemon that is still starting is not reported
     * as usable (SKADI-T-0475). Unauthenticated by design, like liveness.
     */
    /**
     * The APK the daemon is currently offering (SKADI-T-0579): `/app/manifest.json`,
     * the same file the desktop UI's install QR reads. Unauthenticated on the
     * server side because the install page is reachable before pairing.
     */
    suspend fun appManifest(): AppManifest = withContext(Dispatchers.IO) {
        val req = Request.Builder().url("$baseUrl/app/manifest.json").build()
        client.newCall(req).execute().use { resp ->
            check(resp.isSuccessful) { "manifest -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    fun apkUrl(file: String): String = "$baseUrl/app/$file"

    /**
     * Stream `url` to `file`, reporting progress as 0..1 (or -1 when the server
     * sends no length). Writes to a `.part` beside it and renames at the end so a
     * killed download never leaves a truncated APK that looks complete.
     */
    suspend fun downloadTo(url: String, file: java.io.File, onProgress: (Float) -> Unit) =
        withContext(Dispatchers.IO) {
            val part = java.io.File(file.parentFile, file.name + ".part")
            file.parentFile?.mkdirs()
            client.newCall(Request.Builder().url(url).build()).execute().use { resp ->
                check(resp.isSuccessful) { "download -> HTTP ${resp.code}" }
                val body = resp.body!!
                val total = body.contentLength()
                var done = 0L
                body.byteStream().use { input ->
                    part.outputStream().use { out ->
                        val buf = ByteArray(64 * 1024)
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) break
                            out.write(buf, 0, n)
                            done += n
                            onProgress(if (total > 0) (done.toFloat() / total).coerceIn(0f, 1f) else -1f)
                        }
                    }
                }
            }
            file.delete()
            check(part.renameTo(file)) { "could not move ${part.name} into place" }
        }

    suspend fun health(): Boolean = withContext(Dispatchers.IO) {
        runCatching {
            client.newCall(Request.Builder().url("$baseUrl/api/v1/health/ready").build())
                .execute().use { it.isSuccessful }
        }.getOrDefault(false)
    }

    suspend fun listBooks(): List<Book> = withContext(Dispatchers.IO) {
        client.newCall(request("/books")).execute().use { resp ->
            check(resp.isSuccessful) { "books -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    /**
     * `GET /movies` — the movie library (SKADI-T-0575).
     *
     * Paged deliberately. `/movies` unbounded is ~2.8 MB over this library, and
     * SKADI-T-0494 measured that the *bound reaching the database* is what makes
     * it fast — 4 ms at `limit=50` against 49 ms unpaged. A phone on a LAN pays
     * that twice over.
     */
    suspend fun listMovies(limit: Int = 200, offset: Int = 0): List<Movie> =
        withContext(Dispatchers.IO) {
            client.newCall(request("/movies?limit=$limit&offset=$offset")).execute().use { resp ->
                check(resp.isSuccessful) { "movies -> HTTP ${resp.code}" }
                json.decodeFromString(resp.body!!.string())
            }
        }

    /**
     * Every movie, paged (SKADI-T-0580).
     *
     * `listMovies` takes ONE page. Calling it alone showed 200 of this library's
     * 1,818 films and silently dropped the rest — the library looked complete
     * because a full-looking grid is indistinguishable from a truncated one.
     *
     * Paging is safe here because `/movies` orders by `added_at ASC` (repo.rs):
     * a stable order is what stops offset paging from duplicating or skipping
     * rows as pages are fetched.
     *
     * The guard is a page *count*, not a row count, so a server that ignores
     * `limit` cannot spin this forever.
     */
    suspend fun listAllMovies(pageSize: Int = 500): List<Movie> {
        val all = mutableListOf<Movie>()
        var offset = 0
        repeat(40) {
            val page = listMovies(limit = pageSize, offset = offset)
            all += page
            // A short page is the end. Requesting one more to see an empty page
            // would cost a whole round trip on a phone for no information.
            if (page.size < pageSize) return all
            offset += page.size
        }
        return all
    }

    /** Every series, paged — same reasoning as [listAllMovies]. */
    // --- Downloads & activity (SKADI-T-0600) ---

    private suspend inline fun <reified T> getJson(path: String, what: String): T =
        withContext(Dispatchers.IO) {
            client.newCall(request(path)).execute().use { resp ->
                if (resp.code == 401) throw SessionExpiredException()
                if (resp.code == 403) throw NotAllowedException()
                check(resp.isSuccessful) { "$what -> HTTP ${resp.code}" }
                json.decodeFromString<T>(resp.body!!.string())
            }
        }

    /** The member this token belongs to. Cached by the app; refreshed on foreground. */
    suspend fun me(): Me = getJson("/me", "me")

    /**
     * Change your own password (SKADI-T-0626). Signs your *other* devices out
     * and leaves this one signed in.
     *
     * `false` means the current password was wrong; anything else throws.
     */
    suspend fun changePassword(current: String, new: String): Boolean =
        withContext(Dispatchers.IO) {
            val body = buildString {
                append("{\"current\":").append(Json.encodeToString(kotlinx.serialization.serializer<String>(), current))
                append(",\"new\":").append(Json.encodeToString(kotlinx.serialization.serializer<String>(), new)).append("}")
            }
            client.newCall(mutate("POST", "/auth/password", body)).execute().use { resp ->
                when {
                    resp.code == 401 -> false
                    resp.isSuccessful -> true
                    else -> throw failure("change password", resp.code, resp.body?.string())
                }
            }
        }

    /** Every transfer the worker tracks, seeding included; the caller splits. */
    suspend fun listDownloads(limit: Int = 2000): List<Download> =
        getJson("/downloads?limit=$limit", "downloads")

    /** In-flight acquire runs, by stage. */
    suspend fun activity(): List<ActivityRun> = getJson("/activity", "activity")

    suspend fun vpnStatus(): VpnStatus = getJson("/downloads/vpn", "vpn")

    suspend fun workerStatus(): WorkerStatus = getJson("/downloads/worker", "worker")

    suspend fun pauseDownload(id: String): Boolean = send(mutate("POST", "/downloads/$id/pause"))

    suspend fun resumeDownload(id: String): Boolean = send(mutate("POST", "/downloads/$id/resume"))

    /** Remove a transfer; `deleteData` also drops its files. */
    suspend fun removeDownload(id: String, deleteData: Boolean): Boolean =
        send(mutate("DELETE", "/downloads/$id?delete_data=$deleteData"))

    suspend fun listAllSeries(pageSize: Int = 500): List<Series> {
        val all = mutableListOf<Series>()
        var offset = 0
        repeat(40) {
            val page = listSeries(limit = pageSize, offset = offset)
            all += page
            if (page.size < pageSize) return all
            offset += page.size
        }
        return all
    }

    /**
     * Playable URL for an imported movie edition (SKADI-T-0574).
     *
     * Handed straight to ExoPlayer, which issues its own ranged GETs to seek —
     * so this is a URL, not a download.
     *
     * The credential rides as **`?apikey=`**, which is what the daemon's auth
     * actually accepts (auth.rs: Bearer, then `X-Api-Key`, then `?apikey=`). A
     * media player builds its own requests and will not carry our
     * `Authorization` header, so the query form is the only one available — the
     * same reason the ICS feed needs it. It is the most exposed of the three
     * (logs, referrers), which is why the daemon puts it last and why it is used
     * here only where a header is impossible.
     */
    fun movieVideoUrl(movieId: String, editionId: String): String =
        "$baseUrl/api/v1/movies/$movieId/editions/$editionId/video" +
            (token?.takeIf { it.isNotEmpty() }?.let { "?apikey=$it" } ?: "")

    /**
     * `GET /series?view=summary` — the TV library (SKADI-T-0578).
     *
     * The summary projection, not the full shape: SKADI-T-0494 measured `/series`
     * unpaged at 17.5 MB and 304 ms against this library, and the projection at
     * 454 KB. A phone has no business downloading every episode's file path and
     * media info to draw a list of show titles.
     */
    suspend fun listSeries(limit: Int = 500, offset: Int = 0): List<Series> =
        withContext(Dispatchers.IO) {
            client.newCall(request("/series?view=summary&limit=$limit&offset=$offset"))
                .execute().use { resp ->
                    check(resp.isSuccessful) { "series -> HTTP ${resp.code}" }
                    json.decodeFromString(resp.body!!.string())
                }
        }

    /**
     * `GET /series/{id}` — one series with its full episodes.
     *
     * The full shape here on purpose: the episode list needs titles and air
     * dates, which the summary drops. One series is a few hundred KB, not the
     * 17.5 MB a whole-library full fetch would be.
     */
    suspend fun series(id: String): Series = withContext(Dispatchers.IO) {
        client.newCall(request("/series/$id")).execute().use { resp ->
            check(resp.isSuccessful) { "series $id -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    /** Playable URL for an imported episode (SKADI-T-0574). See [movieVideoUrl]. */
    fun episodeVideoUrl(seriesId: String, episodeId: String): String =
        "$baseUrl/api/v1/series/$seriesId/episodes/$episodeId/video" +
            (token?.takeIf { it.isNotEmpty() }?.let { "?apikey=$it" } ?: "")

    suspend fun chapters(bookId: String, fileId: String): List<Chapter> =
        withContext(Dispatchers.IO) {
            client.newCall(request("/books/$bookId/files/$fileId/chapters")).execute().use { resp ->
                check(resp.isSuccessful) { "chapters -> HTTP ${resp.code}" }
                json.decodeFromString(resp.body!!.string())
            }
        }

    /** URL of the audio bytes (Range-resumable, SKADI-T-0329). */
    fun audioUrl(bookId: String, fileId: String): String =
        "$baseUrl/api/v1/books/$bookId/files/$fileId/audio"

    fun authHeader(): Pair<String, String>? =
        token?.takeIf { it.isNotEmpty() }?.let { "Authorization" to "Bearer $it" }

    // --- writes (SKADI-I-0052, library management) ---

    /** Delete a book from the library. `deleteFiles=true` also removes the
     *  imported audio on the server + prunes empty folders (bad-grab cleanup). */
    suspend fun deleteBook(bookId: String, deleteFiles: Boolean): Boolean =
        send(mutate("DELETE", "/books/$bookId?delete_files=$deleteFiles"))

    /** Add (or mark-wanted) a book by ASIN; the hunter acquires it on the next
     *  sweep. Idempotent server-side on an ASIN already present. */
    suspend fun addBook(asin: String, profile: String? = null, search: Boolean = true): Boolean =
        send(mutate("POST", "/books", json.encodeToString(AddBookReq(asin, profile, search))))

    // --- Add media (SKADI-T-0601) ---

    suspend fun lookupMovies(q: String): List<MovieHit> =
        getJson("/movies/lookup?q=${java.net.URLEncoder.encode(q, "UTF-8")}", "movie lookup")

    suspend fun lookupSeries(q: String): List<SeriesHit> =
        getJson("/series/lookup?q=${java.net.URLEncoder.encode(q, "UTF-8")}", "series lookup")

    suspend fun searchBooks(q: String): List<BookHit> =
        getJson("/books/search?q=${java.net.URLEncoder.encode(q, "UTF-8")}", "book search")

    /** Quality profiles, in the daemon's order; the web Add page defaults to the first. */
    suspend fun listProfiles(): List<ProfileSetting> = getJson("/settings/profiles", "profiles")

    suspend fun addMovie(tmdbId: Long, profile: String): Boolean =
        send(mutate("POST", "/movies", json.encodeToString(AddMovieReq(tmdbId, profile))))

    /** `monitor` is `all` (regular episodes; specials stay opt-in), `future` or `missing`. */
    suspend fun addSeries(tvdbId: Long, profile: String, monitor: String = "all"): Boolean =
        send(mutate("POST", "/series", json.encodeToString(AddSeriesReq(tvdbId, profile, monitor))))

    /** All active watchers (author/series/book scopes). */
    suspend fun listWatchers(): List<Watcher> = withContext(Dispatchers.IO) {
        client.newCall(request("/watchers")).execute().use { resp ->
            check(resp.isSuccessful) { "watchers -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    /** Watch a scope ("author"/"series"/"book") by its ASIN — the server backfills
     *  gaps (rate-limited) and auto-acquires future releases. */
    suspend fun setWatcher(scope: String, asin: String): Boolean =
        send(mutate("PUT", "/watchers/$scope/$asin"))

    /** Stop watching a scope. */
    suspend fun clearWatcher(scope: String, asin: String): Boolean =
        send(mutate("DELETE", "/watchers/$scope/$asin"))

    /** Registered authors (name + ASIN) — maps a book's author *name* to the ASIN
     *  a watcher needs (the book payload carries names only). */
    suspend fun listAuthors(): List<AuthorRef> = withContext(Dispatchers.IO) {
        client.newCall(request("/authors")).execute().use { resp ->
            check(resp.isSuccessful) { "authors -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    /** Series completeness rollups (name + series ASIN) — maps a book's series
     *  *name* to the ASIN a series watcher needs (payload carries an internal id). */
    suspend fun listBookSeries(): List<SeriesRollup> = withContext(Dispatchers.IO) {
        client.newCall(request("/audiobooks/series")).execute().use { resp ->
            check(resp.isSuccessful) { "series -> HTTP ${resp.code}" }
            json.decodeFromString(resp.body!!.string())
        }
    }

    /** Re-acquire (mark-wanted / retry) a specific file — the hunter searches +
     *  grabs it on the next pass. Used for a Failed/Missing book on the phone. */
    suspend fun acquireFile(bookId: String, fileId: String): Boolean =
        send(mutate("POST", "/books/$bookId/files/$fileId/acquire"))

    // ------------------------------------------------------------------
    // Hunting (SKADI-T-0602)
    // ------------------------------------------------------------------

    /** Interactive searches wait on every enabled indexer; the daemon caps the
     *  fan-out at ~90 s, so the default 10 s read timeout would cut it short. */
    private val slowClient: OkHttpClient by lazy {
        client.newBuilder().readTimeout(150, java.util.concurrent.TimeUnit.SECONDS).build()
    }

    /** Body of a failed request, as the message a person should read: the
     *  daemon's `{error, message}` envelope when present, else the status. */
    private fun failure(what: String, code: Int, body: String?): IllegalStateException {
        if (code == 401) return SessionExpiredException()
        if (code == 403) return NotAllowedException()
        val detail = body?.let {
            runCatching {
                val o = json.parseToJsonElement(it) as? kotlinx.serialization.json.JsonObject
                (o?.get("message") ?: o?.get("error"))?.let { e ->
                    (e as? kotlinx.serialization.json.JsonPrimitive)?.contentOrNull
                }
            }.getOrNull()
        }
        return IllegalStateException(detail?.takeIf { it.isNotBlank() } ?: "$what -> HTTP $code")
    }

    /** Like [send] but throws with the server's own message on a non-2xx, so a
     *  refused grab or a bad link can be shown rather than just "failed". */
    private suspend fun sendOrThrow(req: Request, what: String): Unit = withContext(Dispatchers.IO) {
        client.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) throw failure(what, resp.code, resp.body?.string())
        }
    }

    /** The backlog: every monitored item with something still unsatisfied. */
    suspend fun wanted(): WantedResponse = getJson("/wanted", "wanted")

    /** Search every indexer for one acquirable ([Acquirable] path). Slow. */
    suspend fun listReleases(path: String): List<ReleaseCandidate> = withContext(Dispatchers.IO) {
        slowClient.newCall(request("$path/releases")).execute().use { resp ->
            if (!resp.isSuccessful) throw failure("search releases", resp.code, resp.body?.string())
            json.decodeFromString<List<ReleaseCandidate>>(resp.body!!.string())
        }
    }

    /** Hand a chosen release to the worker; the row's `release` goes back verbatim. */
    suspend fun grabRelease(path: String, release: JsonElement) =
        sendOrThrow(mutate("POST", "$path/grab", json.encodeToString(GrabReq(release))), "grab")

    /** Attach a magnet / .torrent URL the operator found themselves. */
    suspend fun grabLink(path: String, link: String, title: String? = null) =
        sendOrThrow(mutate("POST", "$path/grab-link", json.encodeToString(GrabLinkReq(link, title))), "grab link")

    /** Queue an automatic search-and-grab on the next hunter pass. */
    suspend fun acquire(path: String) = sendOrThrow(mutate("POST", "$path/acquire"), "search now")

    suspend fun setMovieMonitored(id: String, monitored: Boolean) =
        sendOrThrow(mutate("PATCH", "/movies/$id", json.encodeToString(MonitoredReq(monitored))), "update movie")

    suspend fun setSeriesMonitored(id: String, monitored: Boolean) =
        sendOrThrow(mutate("PATCH", "/series/$id", json.encodeToString(MonitoredReq(monitored))), "update series")

    suspend fun setBookMonitored(id: String, monitored: Boolean) =
        sendOrThrow(mutate("PATCH", "/books/$id", json.encodeToString(MonitoredReq(monitored))), "update book")

    suspend fun monitorSeason(seriesId: String, season: Int, monitored: Boolean) =
        sendOrThrow(mutate("POST", "/series/$seriesId/seasons/$season/monitor", json.encodeToString(MonitoredReq(monitored))), "monitor season")

    suspend fun monitorEpisode(seriesId: String, episodeId: String, monitored: Boolean) =
        sendOrThrow(mutate("POST", "/series/$seriesId/episodes/$episodeId/monitor", json.encodeToString(MonitoredReq(monitored))), "monitor episode")

    /** Every known work by an author or in a series, owned ones marked. */
    suspend fun listWorks(author: String? = null, series: String? = null): List<Work> {
        val q = when {
            author != null -> "author=" + java.net.URLEncoder.encode(author, "UTF-8")
            series != null -> "series=" + java.net.URLEncoder.encode(series, "UTF-8")
            else -> return emptyList()
        }
        return getJson("/audiobooks/works?$q", "works")
    }

    /** Audible author candidates for a name; first is the best match. */
    suspend fun lookupAuthors(name: String): List<AuthorLookupResult> =
        getJson("/authors/lookup?name=" + java.net.URLEncoder.encode(name, "UTF-8"), "author lookup")

    // ------------------------------------------------------------------
    // Server panel (SKADI-T-0606)
    // ------------------------------------------------------------------

    suspend fun daemonHealth(): Health = getJson("/health", "health")
    suspend fun healthChecks(): List<HealthCheck> = getJson("/health/checks", "health checks")
    suspend fun systemStatus(): SystemStatus = getJson("/system/status", "system status")
}
