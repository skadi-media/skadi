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
import com.skadi.core.SkadiApi
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

    private fun foregroundInfo(title: String, p: Float): ForegroundInfo {
        val n = buildNotification(title, p)
        return if (Build.VERSION.SDK_INT >= 34) {
            ForegroundInfo(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            ForegroundInfo(NOTIF_ID, n)
        }
    }

    private fun pushNotification(title: String, p: Float) {
        applicationContext.getSystemService(NotificationManager::class.java)
            ?.notify(NOTIF_ID, buildNotification(title, p))
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
        private const val CHANNEL = "skadi_downloads"
        private const val NOTIF_ID = 4242
        private val JSON = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    }
}
