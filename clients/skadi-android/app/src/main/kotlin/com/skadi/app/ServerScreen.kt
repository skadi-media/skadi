package com.skadi.app

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.skadi.core.Health
import com.skadi.core.HealthCheck
import com.skadi.core.SkadiApi
import com.skadi.core.SystemStatus
import com.skadi.core.VpnStatus
import com.skadi.core.WorkerStatus
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch

private const val POLL_MS = 10_000L

/**
 * The server panel (SKADI-T-0606): one screen that answers "is the stack
 * healthy?" — daemon version and uptime, VPN, worker, every health check
 * with its sentence, the domains and their worker failure counts, and the
 * APK update card. Polled while open; stale-banner when the daemon stops
 * answering.
 */
@Composable
fun ServerScreen(baseUrl: String, token: String?, onBack: () -> Unit) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    var health by remember { mutableStateOf<Health?>(null) }
    var status by remember { mutableStateOf<SystemStatus?>(null) }
    var checks by remember { mutableStateOf<List<HealthCheck>?>(null) }
    var vpn by remember { mutableStateOf<VpnStatus?>(null) }
    var worker by remember { mutableStateOf<WorkerStatus?>(null) }
    var lastGood by remember { mutableStateOf<Long?>(null) }
    var unreachable by remember { mutableStateOf(false) }
    var refreshing by remember { mutableStateOf(false) }
    var tickKey by remember { mutableStateOf(0) }

    suspend fun refresh() {
        val h = runCatching { api.daemonHealth() }
        h.onSuccess { health = it; lastGood = System.currentTimeMillis(); unreachable = false }
            .onFailure { unreachable = true }
        if (h.isSuccess) {
            // Concurrent: the health checks probe every provider and can take
            // many seconds; VPN and worker must not queue behind them.
            coroutineScope {
                launch { runCatching { api.systemStatus() }.onSuccess { status = it } }
                launch { runCatching { api.healthChecks() }.onSuccess { checks = it } }
                launch { runCatching { api.vpnStatus() }.onSuccess { vpn = it } }
                launch { runCatching { api.workerStatus() }.onSuccess { worker = it } }
            }
        }
    }
    PollEffect(key = tickKey, baseMs = POLL_MS) { refresh(); refreshing = false }

    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "Server", onBack = onBack)
        if (unreachable) StaleBanner(lastGood)
        Refreshable(refreshing = refreshing, onRefresh = { refreshing = true; tickKey++ }) {
            Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                val h = health
                val st = status
                if (h == null && !unreachable) {
                    Loading()
                    return@Column
                }
                Section("Daemon")
                Line("Version", h?.version ?: "—")
                Line("Up for", st?.let { humanDuration(it.uptimeSeconds) } ?: "—")
                Line("Database", st?.database ?: "—")
                st?.libraryRoot?.let { Line("Library root", it) }
                Section("Network")
                Line(
                    "VPN",
                    when {
                        vpn == null -> "—"
                        !vpn!!.reachable -> "unreachable"
                        !vpn!!.connected -> "down"
                        else -> listOfNotNull(vpn!!.city, vpn!!.country).joinToString(", ").ifEmpty { "connected" }
                    },
                    bad = vpn != null && (!vpn!!.reachable || !vpn!!.connected),
                )
                Line(
                    "Worker",
                    when {
                        worker == null -> "—"
                        worker!!.stale -> "not responding"
                        else -> "ok" + (worker!!.free_bytes?.let { " · ${humanBytes(it)} free" } ?: "")
                    },
                    bad = worker?.stale == true,
                )
                Section("Health checks")
                val cs = checks
                if (cs == null) {
                    Text("…", modifier = Modifier.padding(horizontal = 16.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
                } else {
                    cs.forEach { c -> Line(c.name, c.detail.ifEmpty { c.status }, bad = c.status == "fail", warn = c.status == "warn") }
                }
                if (st != null && st.domains.isNotEmpty()) {
                    Section("Domains")
                    st.domains.forEach { d ->
                        Line(
                            d.name,
                            (if (d.enabled) "enabled" else "disabled") +
                                (d.workerFailures.takeIf { it > 0 }?.let { " · $it worker failures" } ?: ""),
                            bad = d.workerFailures > 0,
                        )
                    }
                }
                HorizontalDivider(modifier = Modifier.padding(vertical = 8.dp))
                Section("App")
                Line("Installed", "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})")
                Line(
                    "Offered",
                    UpdateCheck.offered?.let { "${it.versionName} (${it.versionCode})" }
                        ?: (UpdateCheck.lastError?.let { "couldn't check: $it" } ?: "—"),
                    bad = UpdateCheck.lastError != null,
                )
                TextButton(
                    onClick = { UpdateCheck.nonce.value++ },
                    modifier = Modifier.padding(horizontal = 8.dp),
                ) { Text("Check for updates") }
                UpdateCard(api)
            }
        }
    }
}

@Composable
private fun Section(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 4.dp),
    )
}

@Composable
private fun Line(label: String, value: String, bad: Boolean = false, warn: Boolean = false) {
    Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.weight(0.38f))
        Text(
            value,
            style = MaterialTheme.typography.bodyMedium,
            color = when {
                bad -> MaterialTheme.colorScheme.error
                warn -> MaterialTheme.colorScheme.tertiary
                else -> MaterialTheme.colorScheme.onSurface
            },
            modifier = Modifier.weight(0.62f),
        )
    }
}
