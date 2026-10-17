package com.skadi.app

import android.content.Context
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import com.skadi.core.Book
import com.skadi.core.OfflineVideoStore
import com.skadi.core.VideoDownloadRequest
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/**
 * Download coordinator. Downloads run in a WorkManager data-sync FOREGROUND SERVICE
 * ([DownloadWorker], SKADI-T-0358) so the screen turning off (doze) or the app being
 * swiped away can't abort the transfer — the previous app-scoped CoroutineScope
 * survived rotation/backgrounding but got suspended on screen-sleep, resetting the
 * connection (it resumed from the `.part` only when the screen came back).
 *
 * The worker runs in-process and calls back into these StateFlows, which the library
 * UI observes — so the progress/error surface is unchanged.
 */
object DownloadCenter {
    /** fid -> progress 0..1 while running; absent = not downloading. */
    private val _progress = MutableStateFlow<Map<String, Float>>(emptyMap())
    val progress: StateFlow<Map<String, Float>> = _progress

    /** fid -> error message from the last failed attempt (cleared on retry). */
    private val _errors = MutableStateFlow<Map<String, String>>(emptyMap())
    val errors: StateFlow<Map<String, String>> = _errors

    /** Bumped when a video download is added or removed, so lists re-read the store. */
    private val _videoChanges = MutableStateFlow(0)
    val videoChanges: StateFlow<Int> = _videoChanges

    fun videosChanged() {
        _videoChanges.value += 1
    }

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    /** Enqueue a doze-proof download as unique work (per fid, so a double-tap can't
     *  double-download). The book travels to the worker as JSON inputData. */
    fun start(context: Context, book: Book, fid: String) {
        if (_progress.value.containsKey(fid)) return // already running/queued
        _progress.value += (fid to 0f)
        _errors.value -= fid
        val data = Data.Builder()
            .putString(DownloadWorker.KEY_FID, fid)
            .putString(DownloadWorker.KEY_BOOK, json.encodeToString(book))
            .build()
        val req = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setInputData(data)
            .setConstraints(
                Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build(),
            )
            .build()
        WorkManager.getInstance(context)
            .enqueueUniqueWork("dl:$fid", ExistingWorkPolicy.KEEP, req)
    }

    /** Ids cancelled by the viewer, so the worker deletes rather than keeps the part. */
    private val cancelled = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()

    /**
     * Download a film or an episode (SKADI-T-0637). Keyed by its video progress
     * key, so the same id names the download, the file and the saved position.
     * Same rules as a book: any connection (the UI says Wi-Fi), resumable,
     * one at a time per item.
     */
    fun startVideo(context: Context, req: VideoDownloadRequest) {
        val id = req.key
        if (_progress.value.containsKey(id)) return
        cancelled -= id
        _progress.value += (id to 0f)
        _errors.value -= id
        val data = Data.Builder()
            .putString(DownloadWorker.KEY_VIDEO, json.encodeToString(req))
            .build()
        val work = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setInputData(data)
            .setConstraints(
                Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build(),
            )
            .build()
        WorkManager.getInstance(context)
            .enqueueUniqueWork("dl:$id", ExistingWorkPolicy.KEEP, work)
    }

    /**
     * Stop a video download and give its bytes back (SKADI-T-0637). A running
     * worker sees the cancel at its next chunk and deletes the part itself;
     * one still waiting for a network never ran, so the part goes here.
     */
    fun cancelVideo(context: Context, id: String) {
        cancelled += id
        WorkManager.getInstance(context).cancelUniqueWork("dl:$id")
        _progress.value -= id
        _errors.value -= id
        val store = OfflineVideoStore(context.filesDir)
        kotlin.concurrent.thread(name = "skadi-cancel") {
            if (!store.isDownloaded(id)) store.delete(id)
            videosChanged()
        }
    }

    fun wasCancelled(id: String): Boolean = id in cancelled

    // --- called by DownloadWorker (same process) to reflect state into the UI ---
    fun markStart(fid: String) {
        _progress.value += (fid to (_progress.value[fid] ?: 0f))
        _errors.value -= fid
    }

    fun updateProgress(fid: String, p: Float) {
        _progress.value += (fid to p)
    }

    fun markError(fid: String, msg: String) {
        _errors.value += (fid to msg)
    }

    fun markDone(fid: String) {
        _progress.value -= fid
        videosChanged()
    }
}
