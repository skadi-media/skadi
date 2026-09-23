package com.skadi.app

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.skadi.core.ActivityRun
import com.skadi.core.Download
import com.skadi.core.SkadiApi
import com.skadi.core.VpnStatus
import com.skadi.core.WorkerStatus
import kotlinx.coroutines.launch
import java.time.Instant

/** How often the tab re-polls the daemon while it is on screen. */
private const val POLL_MS = 3_000L

/**
 * Downloads tab (SKADI-T-0600): the controller view. Live transfers with
 * pause / resume / remove, the hunter's in-flight runs by stage, and a server
 * line (VPN, worker) so a stuck stack reads as stuck rather than as "nothing
 * to do". Polls every [POLL_MS] while composed; each poll is best-effort so a
 * flaky link degrades to stale numbers, not a blank page.
 */
@Composable
fun DownloadsScreen(baseUrl: String, token: String?, onWanted: () -> Unit = {}, onServer: () -> Unit = {}) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val scope = rememberCoroutineScope()
    var downloads by remember { mutableStateOf<List<Download>?>(null) }
    var runs by remember { mutableStateOf<List<ActivityRun>>(emptyList()) }
    var vpn by remember { mutableStateOf<VpnStatus?>(null) }
    var worker by remember { mutableStateOf<WorkerStatus?>(null) }
    var failed by remember { mutableStateOf(false) }
    var removeTarget by remember { mutableStateOf<Download?>(null) }
    var lastGood by remember { mutableStateOf<Long?>(null) }
    var unreachable by remember { mutableStateOf(false) }
    var refreshing by remember { mutableStateOf(false) }
    var tickKey by remember { mutableStateOf(0) }

    suspend fun refresh() {
        val r = runCatching { api.listDownloads() }
        r.onSuccess { downloads = it; failed = false; unreachable = false; lastGood = System.currentTimeMillis() }
            .onFailure { failed = downloads == null; unreachable = true }
        runCatching { api.activity() }.onSuccess { runs = it }
        runCatching { api.vpnStatus() }.onSuccess { vpn = it }
        runCatching { api.workerStatus() }.onSuccess { worker = it }
    }

    // Lifecycle- and network-aware (SKADI-T-0606): nothing while backgrounded,
    // four times slower on cellular; pull-to-refresh forces a tick.
    PollEffect(key = tickKey, baseMs = POLL_MS) { refresh(); refreshing = false }

    removeTarget?.let { d ->
        AlertDialog(
            onDismissRequest = { removeTarget = null },
            title = { Text("Remove download?") },
            text = { Text(d.title, maxLines = 3, overflow = TextOverflow.Ellipsis) },
            confirmButton = {
                TextButton(onClick = {
                    removeTarget = null
                    scope.launch { api.removeDownload(d.id, deleteData = true); refresh() }
                }) { Text("Remove and delete files") }
            },
            dismissButton = {
                Row {
                    TextButton(onClick = {
                        removeTarget = null
                        scope.launch { api.removeDownload(d.id, deleteData = false); refresh() }
                    }) { Text("Remove, keep files") }
                    TextButton(onClick = { removeTarget = null }) { Text("Cancel") }
                }
            },
        )
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            SkadiTopBar(title = "Downloads") {
                // The backlog lives behind the controller tab (SKADI-T-0602).
                TextButton(onClick = onWanted) { Text("Wanted") }
            }
        },
    ) { inset ->
        val all = downloads
        when {
            all == null && failed -> EmptyState("Couldn't reach the server.") { TextButton(onClick = { tickKey++ }) { Text("Try again") } }
            all == null -> Loading()
            else -> {
                val active = all.filter { !it.isSeeding }
                    .sortedWith(compareByDescending<Download> { it.percent }.thenBy { it.title })
                val seeding = all.count { it.isSeeding }
                val hunting = runs.sortedByDescending { it.started_at ?: "" }
Column(modifier = Modifier.fillMaxSize().padding(inset)) {
                if (unreachable) StaleBanner(lastGood)
                Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; tickKey++ }) {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(bottom = 24.dp),
                ) {
                    item { ServerLine(vpn, worker, seeding, onClick = onServer) }
                    item { SectionTitle("Transfers", active.size) }
                    if (active.isEmpty()) {
                        item { Muted("Nothing transferring.") }
                    }
                    items(active, key = { it.id }) { d ->
                        TransferRow(
                            d = d,
                            onPauseResume = {
                                scope.launch {
                                    if (d.isPaused) api.resumeDownload(d.id) else api.pauseDownload(d.id)
                                    refresh()
                                }
                            },
                            onRemove = { removeTarget = d },
                        )
                    }
                    item { SectionTitle("Hunting", hunting.size) }
                    if (hunting.isEmpty()) {
                        item { Muted("No searches in flight.") }
                    }
                    items(hunting, key = { it.run_id }) { RunRow(it) }
                }
                }
                }
            }
        }
    }
}

@Composable
private fun ServerLine(vpn: VpnStatus?, worker: WorkerStatus?, seeding: Int, onClick: () -> Unit = {}) {
    val ok = MaterialTheme.colorScheme.primary
    val bad = MaterialTheme.colorScheme.error
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    // Tapping opens the server panel (SKADI-T-0606).
    Column(modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 16.dp, vertical = 8.dp)) {
        val vpnText = when {
            vpn == null -> "VPN: …"
            !vpn.reachable -> "VPN: unreachable"
            !vpn.connected -> "VPN: down"
            else -> "VPN: " + listOfNotNull(vpn.city, vpn.country).joinToString(", ").ifEmpty { "connected" }
        }
        val vpnColor = if (vpn?.connected == true) ok else if (vpn == null) muted else bad
        Text(vpnText, style = MaterialTheme.typography.bodyMedium, color = vpnColor)
        val workerText = when {
            worker == null -> "Worker: …"
            worker.stale -> "Worker: not responding"
            else -> "Worker: ok · " + (worker.free_bytes?.let { humanBytes(it) + " free" } ?: "") +
                " · seeding $seeding"
        }
        val workerColor = if (worker == null) muted else if (worker.stale) bad else ok
        Text(workerText, style = MaterialTheme.typography.bodyMedium, color = workerColor)
        Text("Server details ›", style = MaterialTheme.typography.labelMedium, color = muted)
    }
}

@Composable
private fun SectionTitle(label: String, count: Int) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.width(8.dp))
        Text(count.toString(), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun Muted(text: String) {
    Text(
        text,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun TransferRow(d: Download, onPauseResume: () -> Unit, onRemove: () -> Unit) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(modifier = Modifier.weight(1f)) {
                Text(d.title, style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(
                    transferLine(d),
                    style = MaterialTheme.typography.bodySmall,
                    color = if (d.error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton(onClick = onPauseResume) {
                Icon(
                    if (d.isPaused) Icons.Filled.PlayArrow else Icons.Filled.Pause,
                    contentDescription = if (d.isPaused) "Resume" else "Pause",
                )
            }
            IconButton(onClick = onRemove) {
                Icon(Icons.Filled.Delete, contentDescription = "Remove")
            }
        }
        Spacer(Modifier.height(4.dp))
        LinearProgressIndicator(
            progress = { (d.percent / 100.0).toFloat().coerceIn(0f, 1f) },
            modifier = Modifier.fillMaxWidth().height(4.dp),
            trackColor = MaterialTheme.colorScheme.surfaceVariant,
        )
    }
}

@Composable
private fun RunRow(r: ActivityRun) {
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp)) {
        Text(r.title, style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
        val parts = mutableListOf(r.current_stage, r.kind.lowercase())
        r.started_at?.let { ago(it) }?.let { parts += "since $it" }
        r.candidates_considered?.let { parts += "$it candidates" }
        Text(
            parts.joinToString(" · "),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** The one-line status under a transfer: what is happening and how fast. */
fun transferLine(d: Download): String {
    d.error?.let { return it }
    val pct = "${d.percent.toInt()}%"
    return when {
        d.isPaused -> "$pct · paused"
        d.status == "queued" -> "queued"
        d.status == "stalled" -> "$pct · stalled"
        d.status == "error" -> "$pct · error"
        (d.down_speed_bps ?: 0) > 0 -> {
            val parts = mutableListOf(pct, humanRate(d.down_speed_bps!!))
            d.peers?.let { parts += "$it peer${if (it == 1) "" else "s"}" }
            d.eta_seconds?.takeIf { it > 0 }?.let { parts += "ETA " + humanDuration(it) }
            parts.joinToString(" · ")
        }
        (d.peers ?: 0) > 0 -> "$pct · ${d.peers} peer${if (d.peers == 1) "" else "s"} · no data yet"
        (d.peers_seen ?: 0) > 0 -> "$pct · waiting for peers · ${d.peers_seen} seen"
        d.total_bytes == 0L -> "fetching metadata"
        else -> "$pct · waiting for peers"
    }
}

fun humanBytes(b: Long): String {
    val units = listOf("B", "KB", "MB", "GB", "TB")
    var v = b.toDouble()
    var i = 0
    while (v >= 1000 && i < units.lastIndex) { v /= 1000; i++ }
    return if (i == 0) "$b B" else String.format("%.1f %s", v, units[i])
}

fun humanRate(bps: Long): String = humanBytes(bps) + "/s"

fun humanDuration(secs: Long): String = when {
    secs < 60 -> "${secs}s"
    secs < 3600 -> "${secs / 60}m"
    secs < 86400 -> "${secs / 3600}h ${(secs % 3600) / 60}m"
    else -> "${secs / 86400}d"
}

/** `5m` / `2h` / `3d` ago for an RFC 3339 timestamp; null when unparseable. */
fun ago(iso: String): String? = runCatching {
    val secs = (Instant.now().epochSecond - Instant.parse(iso).epochSecond).coerceAtLeast(0)
    humanDuration(secs)
}.getOrNull()
