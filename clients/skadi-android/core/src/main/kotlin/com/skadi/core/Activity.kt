package com.skadi.core

import kotlinx.serialization.Serializable

/**
 * The daemon's live view of transfers and hunting (SKADI-T-0600): what the
 * Downloads tab renders. Field names mirror the wire JSON (snake_case) like
 * the other models; unknown fields are ignored so the daemon can grow.
 */

/** One row of `GET /downloads`. `acquirable_ref` carries the release title. */
@Serializable
data class Download(
    val id: String,
    val acquirable_ref: String,
    val info_hash: String? = null,
    /** `downloading` / `paused` / `queued` / `seeding` / `completed` / `failed` … */
    val status: String,
    val progress_bytes: Long = 0,
    val total_bytes: Long = 0,
    val percent: Double = 0.0,
    val down_speed_bps: Long? = null,
    val up_speed_bps: Long? = null,
    val uploaded_bytes: Long? = null,
    val ratio: Double? = null,
    val peers: Int? = null,
    val peers_seen: Int? = null,
    val eta_seconds: Long? = null,
    val error: String? = null,
) {
    val title: String get() = acquirable_ref
    val isSeeding: Boolean get() = status == "seeding"
    val isPaused: Boolean get() = status == "paused"
}

@Serializable
data class TransferWatch(
    val since: String? = null,
    val best_progress: Double? = null,
    val progress_at: String? = null,
    val live_since: String? = null,
)

/** One in-flight acquire run from `GET /activity`. */
@Serializable
data class ActivityRun(
    val run_id: String,
    val kind: String,
    val acquirable_ref: String,
    val started_at: String? = null,
    /** `searching` / `deciding` / `grabbing` / `downloading` / `importing` / `notifying`. */
    val current_stage: String,
    val chosen_title: String? = null,
    val candidates_considered: Int? = null,
    val decision: String? = null,
    val last_seen: String? = null,
    val transfer: TransferWatch? = null,
) {
    val title: String get() = chosen_title ?: acquirable_ref
}

/** `GET /downloads/vpn`. */
@Serializable
data class VpnStatus(
    val reachable: Boolean = false,
    val connected: Boolean = false,
    val exit_ip: String? = null,
    val country: String? = null,
    val city: String? = null,
)

/** `GET /downloads/worker`. */
@Serializable
data class WorkerStatus(
    val running: Boolean = false,
    val worker_id: String? = null,
    val last_seen_at: String? = null,
    val age_secs: Long? = null,
    val stale_after_secs: Long? = null,
    val free_bytes: Long? = null,
    val total_bytes: Long? = null,
) {
    val stale: Boolean get() = !running || (age_secs != null && stale_after_secs != null && age_secs > stale_after_secs)
}
