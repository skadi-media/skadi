package com.skadi.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.pm.ServiceInfo
import android.os.Build
import androidx.work.CoroutineWorker
import androidx.work.ForegroundInfo
import androidx.work.WorkerParameters
import com.skadi.core.Book
import com.skadi.core.Downloader
import com.skadi.core.OfflineStore
import com.skadi.core.OfflineVideoStore
import com.skadi.core.ResumableFetch
import com.skadi.core.SkadiApi
import com.skadi.core.VideoDownloadRequest
import com.skadi.core.VideoDownloader
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.Json

/**
 * Foreground-service download worker (SKADI-T-0358). A book download runs inside a
 * WorkManager data-sync foreground service so the screen turning off (doze) can't
 * abort the transfer — the old app-scoped coroutine got suspended on sleep, which
 * reset the connection (it only resumed from the `.part` once the screen woke).
 *
 * The worker runs in-process, so it keeps updating [DownloadCenter]'s StateFlows —
 * the library UI's source of truth — with no UI rewiring. The book + fid arrive as
 * inputData; the server (base URL + token) comes from [Settings].
 */
class DownloadWorker(ctx: Context, params: WorkerParameters) : CoroutineWorker(ctx, params) {
    override suspend fun doWork(): Result {
        inputData.getString(KEY_VIDEO)?.let { return doVideo(it) }
        val fid = inputData.getString(KEY_FID) ?: return Result.failure()
        val bookJson = inputData.getString(KEY_BOOK) ?: return Result.failure()
        val book = runCatching { JSON.decodeFromString<Book>(bookJson) }.getOrNull()
            ?: return Result.failure()
        val server = Settings.server(applicationContext).first() ?: run {
            DownloadCenter.markError(fid, "not paired")
            return Result.failure()
        }
        val store = OfflineStore(applicationContext.filesDir)
        val downloader = Downloader(SkadiApi(server.baseUrl, server.token), store)

        setForeground(foregroundInfo(book.title, 0f))
        DownloadCenter.markStart(fid)
        val result = downloader.download(book, fid) { p ->
            DownloadCenter.updateProgress(fid, p)
            runCatching { pushNotification(book.title, p) }
        }
        DownloadCenter.markDone(fid)
        return result.fold(
            onSuccess = { Result.success() },
            onFailure = { e ->
                DownloadCenter.markError(fid, e.message ?: "download failed")
                // WorkManager reschedules (with backoff) — combined with the .part
                // resume, a dropped connection continues rather than restarts.
                Result.retry()
            },
        )
    }

    /**
     * A film or an episode (SKADI-T-0637): the same foreground service, the
     * same retry-and-resume, its own notification so a book and a film
     * downloading together do not share one bar.
     */
    private suspend fun doVideo(reqJson: String): Result {
        val req = runCatching { JSON.decodeFromString<VideoDownloadRequest>(reqJson) }.getOrNull()
            ?: return Result.failure()
        val key = req.key
        val server = Settings.server(applicationContext).first() ?: run {
            DownloadCenter.markError(key, "not paired")
            return Result.failure()
        }
        val store = OfflineVideoStore(applicationContext.filesDir)
        val downloader = VideoDownloader(SkadiApi(server.baseUrl, server.token), store)
        val label = req.subtitle?.let { "${req.title} · ${it.substringBefore(" · ")}" } ?: req.title
        val notif = NOTIF_ID + 1 + (key.hashCode() and 0xFFFF)

        setForeground(foregroundInfo(label, 0f, notif))
        DownloadCenter.markStart(key)
        try {
            val result = downloader.download(req) { p ->
                DownloadCenter.updateProgress(key, p)
                runCatching { pushNotification(label, p, notif) }
            }
            if (DownloadCenter.wasCancelled(key)) return Result.failure()
            return result.fold(
                onSuccess = { Result.success() },
                onFailure = { e ->
                    if (isStopped) return Result.retry()
                    DownloadCenter.markError(key, e.message ?: "download failed")
                    // Out of space does not fix itself by retrying.
                    if (e is ResumableFetch.NoSpaceException) Result.failure() else Result.retry()
                },
            )
        } finally {
            DownloadCenter.markDone(key)
            // Cancelled by the viewer: give the bytes back. Stopped by the
            // system (the network went, say) is not a cancel: the part stays
            // so the next run resumes it.
            if (DownloadCenter.wasCancelled(key)) store.delete(key)
        }
    }

    private fun foregroundInfo(title: String, p: Float, id: Int = NOTIF_ID): ForegroundInfo {
        val n = buildNotification(title, p)
        return if (Build.VERSION.SDK_INT >= 34) {
            ForegroundInfo(id, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            ForegroundInfo(id, n)
        }
    }

    private fun pushNotification(title: String, p: Float, id: Int = NOTIF_ID) {
        applicationContext.getSystemService(NotificationManager::class.java)
            ?.notify(id, buildNotification(title, p))
    }

    private fun buildNotification(title: String, p: Float): Notification {
        val nm = applicationContext.getSystemService(NotificationManager::class.java)
        if (nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "Downloads", NotificationManager.IMPORTANCE_LOW),
            )
        }
        val pct = (p * 100).toInt().coerceIn(0, 100)
        return Notification.Builder(applicationContext, CHANNEL)
            .setContentTitle("Downloading")
            .setContentText(title)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setOngoing(true)
            .setProgress(100, pct, p <= 0f) // indeterminate until the first byte
            .build()
    }

    companion object {
        const val KEY_FID = "fid"
        const val KEY_BOOK = "book"
        const val KEY_VIDEO = "video"
        private const val CHANNEL = "skadi_downloads"
        private const val NOTIF_ID = 4242
        private val JSON = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    }
}
