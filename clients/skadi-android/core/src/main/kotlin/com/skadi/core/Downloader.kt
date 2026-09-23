package com.skadi.core

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.RandomAccessFile
import java.util.concurrent.TimeUnit

/** OkHttp default read timeout is 10s — fine for a request/response, but it
 *  trips on any 10s gap between chunks of a multi-GB streamed audiobook over
 *  slow Wi-Fi, resetting the connection mid-transfer (the download then
 *  "interrupts halfway" and resumes from the .part — SKADI-T-0354). A streamed
 *  download wants NO idle-read cap; keep a sane connect timeout. */
private fun downloadClient(): OkHttpClient =
    OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .callTimeout(0, TimeUnit.MILLISECONDS)
        .build()

/**
 * Book downloader (SKADI-T-0342, hardened in the second review pass): audio
 * via the Range endpoint (SKADI-T-0329) with RESUME — a partial `.part` file
 * continues with `bytes={len}-`. Integrity rules:
 *  - a non-resumed (200) restart TRUNCATES the .part first (stale tail bytes
 *    must never be promoted into a "complete" m4b);
 *  - a 206 must confirm `Content-Range` starts exactly at our offset, else we
 *    restart from scratch;
 *  - HTTP 416 (our .part already covers the file — e.g. killed between the
 *    write loop and the rename) clears the .part and retries once fresh;
 *  - the rename to audio.m4b happens ONLY when written == expected total.
 * meta.json is written LAST, so its presence means "complete".
 */
class Downloader(
    private val api: SkadiApi,
    private val store: OfflineStore,
    private val client: OkHttpClient = downloadClient(),
) {
    suspend fun download(
        book: Book,
        fid: String,
        onProgress: (Float) -> Unit,
    ): Result<Unit> = withContext(Dispatchers.IO) {
        runCatching {
            store.saveChapters(
                fid,
                runCatching { api.chapters(book.id, fid) }.getOrDefault(emptyList()),
            )
            book.coverUrl?.takeIf { it.isNotEmpty() }?.let { url ->
                runCatching {
                    client.newCall(Request.Builder().url(url).build()).execute().use { r ->
                        if (r.isSuccessful) r.body?.byteStream()?.use { ins ->
                            store.coverFile(fid).outputStream().use { ins.copyTo(it) }
                        }
                    }
                }
            }
            fetchAudio(book, fid, onProgress, retryOn416 = true)
            store.saveMeta(
                OfflineBook(
                    bookId = book.id,
                    fileId = fid,
                    title = book.title,
                    authors = book.authors,
                    narrators = book.narrators,
                    seriesName = book.series?.name,
                    seriesPosition = book.series?.position,
                    sizeBytes = store.audioFile(fid).length(),
                ),
            )
            onProgress(1f)
        }
    }

    private fun fetchAudio(
        book: Book,
        fid: String,
        onProgress: (Float) -> Unit,
        retryOn416: Boolean,
    ) {
        val part = store.partFile(fid)
        part.parentFile?.mkdirs()
        val have = if (part.isFile) part.length() else 0L

        val req = Request.Builder().url(api.audioUrl(book.id, fid)).apply {
            api.authHeader()?.let { (k, v) -> header(k, v) }
            if (have > 0) header("Range", "bytes=$have-")
        }.build()

        client.newCall(req).execute().use { resp ->
            if (resp.code == 416) {
                // Our .part already spans (or overran) the file — killed after
                // the loop but before the rename, or the server file shrank.
                // Clear and go again from zero, exactly once.
                check(retryOn416) { "audio -> HTTP 416 twice; giving up" }
                part.delete()
                return fetchAudio(book, fid, onProgress, retryOn416 = false)
            }
            check(resp.isSuccessful) { "audio -> HTTP ${resp.code}" }
            val body = resp.body ?: error("empty audio body")

            val resumed = resp.code == 206
            var start = 0L
            var total = body.contentLength().takeIf { it > 0 } ?: -1L
            if (resumed) {
                // "bytes START-END/TOTAL" — START must be our offset, or the
                // server isn't giving us what we asked for: restart clean.
                val cr = resp.header("Content-Range").orEmpty()
                val m = Regex("bytes (\\d+)-(\\d+)/(\\d+)").find(cr)
                val crStart = m?.groupValues?.get(1)?.toLongOrNull()
                val crTotal = m?.groupValues?.get(3)?.toLongOrNull()
                if (crStart != have) {
                    part.delete()
                    check(retryOn416) { "server ignored our resume offset twice" }
                    return fetchAudio(book, fid, onProgress, retryOn416 = false)
                }
                start = have
                if (crTotal != null) total = crTotal
            }

            RandomAccessFile(part, "rw").use { raf ->
                if (!resumed) raf.setLength(0) // stale tail bytes must die
                raf.seek(start)
                val src = body.byteStream()
                val buf = ByteArray(256 * 1024)
                var written = start
                while (true) {
                    val n = src.read(buf)
                    if (n < 0) break
                    raf.write(buf, 0, n)
                    written += n
                    if (total > 0) {
                        onProgress((written.toDouble() / total).toFloat().coerceIn(0f, 1f))
                    }
                }
                if (total > 0) {
                    check(written == total) {
                        "audio truncated: $written of $total bytes (will resume)"
                    }
                }
            }
        }
        check(part.renameTo(store.audioFile(fid))) { "rename failed" }
    }
}
