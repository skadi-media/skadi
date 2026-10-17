package com.skadi.core

import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.File
import java.io.RandomAccessFile
import java.util.concurrent.CancellationException

/**
 * One resumable file fetch over Range, shared by books and video (SKADI-T-0637).
 * The rules are the ones the book downloader has had since SKADI-T-0342:
 *  - a partial [part] file continues with `bytes={len}-`;
 *  - a 200 (no resume) truncates the part first, so stale tail bytes never
 *    end up in a "complete" file;
 *  - a 206 must start exactly at our offset, else start again from zero;
 *  - HTTP 416 (the part already covers the file) clears the part and tries
 *    once more from zero;
 *  - the rename to [target] happens only when written == expected total.
 */
object ResumableFetch {
    /** Free space kept back when a download is checked against the disk. */
    const val RESERVE_BYTES: Long = 256L * 1024 * 1024

    /** `bytes START-END/TOTAL` → (start, total), or null when it does not parse. */
    fun parseContentRange(header: String?): Pair<Long, Long?>? {
        val m = Regex("bytes (\\d+)-(\\d+)/(\\d+|\\*)").find(header.orEmpty()) ?: return null
        val start = m.groupValues[1].toLongOrNull() ?: return null
        return start to m.groupValues[3].toLongOrNull()
    }

    /** The Range header for a part file of [have] bytes, or null to fetch it all. */
    fun rangeHeader(have: Long): String? = if (have > 0) "bytes=$have-" else null

    /**
     * Whether [remaining] more bytes fit in [free] bytes of free space with
     * [RESERVE_BYTES] kept back. An unknown size (< 0) is let through: the
     * server did not say, and the write fails cleanly if the disk fills.
     */
    fun fits(remaining: Long, free: Long, reserve: Long = RESERVE_BYTES): Boolean =
        remaining < 0 || remaining + reserve <= free

    class NoSpaceException(val needed: Long, val free: Long) :
        IllegalStateException("not enough space on this phone: needs ${needed / MB} MB, ${free / MB} MB free")

    private const val MB = 1024L * 1024

    fun fetch(
        client: OkHttpClient,
        url: String,
        authHeader: Pair<String, String>?,
        part: File,
        target: File,
        onProgress: (Float) -> Unit,
        isCancelled: () -> Boolean = { false },
        freeSpace: () -> Long = { part.parentFile?.usableSpace ?: Long.MAX_VALUE },
        what: String = "file",
        retry: Boolean = true,
    ) {
        part.parentFile?.mkdirs()
        val have = if (part.isFile) part.length() else 0L

        val req = Request.Builder().url(url).apply {
            authHeader?.let { (k, v) -> header(k, v) }
            rangeHeader(have)?.let { header("Range", it) }
        }.build()

        client.newCall(req).execute().use { resp ->
            if (resp.code == 416) {
                check(retry) { "$what -> HTTP 416 twice; giving up" }
                part.delete()
                return fetch(client, url, authHeader, part, target, onProgress, isCancelled, freeSpace, what, retry = false)
            }
            check(resp.isSuccessful) { "$what -> HTTP ${resp.code}" }
            val body = resp.body ?: error("empty $what body")

            val resumed = resp.code == 206
            var start = 0L
            var total = body.contentLength().takeIf { it > 0 } ?: -1L
            if (resumed) {
                val cr = parseContentRange(resp.header("Content-Range"))
                if (cr?.first != have) {
                    part.delete()
                    check(retry) { "server ignored our resume offset twice" }
                    return fetch(client, url, authHeader, part, target, onProgress, isCancelled, freeSpace, what, retry = false)
                }
                start = have
                total = cr.second ?: (if (total > 0) have + total else -1L)
            }

            val remaining = if (total > 0) total - start else -1L
            val free = freeSpace()
            if (!fits(remaining, free)) throw NoSpaceException(remaining, free)

            RandomAccessFile(part, "rw").use { raf ->
                if (!resumed) raf.setLength(0) // stale tail bytes must die
                raf.seek(start)
                val src = body.byteStream()
                val buf = ByteArray(256 * 1024)
                var written = start
                while (true) {
                    if (isCancelled()) throw CancellationException("$what download cancelled")
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
                        "$what truncated: $written of $total bytes (will resume)"
                    }
                }
            }
        }
        target.delete()
        check(part.renameTo(target)) { "rename failed" }
    }
}
