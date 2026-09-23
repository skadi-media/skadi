package com.skadi.app

import android.content.Context
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import com.skadi.core.Book
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
    }
}
