package com.skadi.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay

/**
 * Controller polish (SKADI-T-0606): the two habits every list screen shares.
 *
 * [Refreshable] is pull-to-refresh around a scrolling region. [PollEffect]
 * is a polling loop that only runs while the screen is started (nothing
 * spins in the background) and slows down on a metered connection — a phone
 * on cellular asking every three seconds "is it downloading?" is paying for
 * an answer that changes once a minute.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Refreshable(
    refreshing: Boolean,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier.fillMaxSize(),
    content: @Composable () -> Unit,
) {
    PullToRefreshBox(isRefreshing = refreshing, onRefresh = onRefresh, modifier = modifier) {
        Box(modifier = Modifier.fillMaxSize()) { content() }
    }
}

/** Is the active network metered (cellular, or a Wi-Fi the OS marks as such)? */
fun isMetered(context: Context): Boolean {
    val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager ?: return false
    val caps = cm.getNetworkCapabilities(cm.activeNetwork) ?: return false
    return !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
}

/** How long to wait between polls: [baseMs] on unmetered, [meteredMs] on metered. */
fun pollInterval(context: Context, baseMs: Long, meteredMs: Long = baseMs * 4): Long =
    if (isMetered(context)) meteredMs else baseMs

/**
 * Run [tick] now and then every poll interval while the lifecycle is at
 * least STARTED; suspend (not just skip) while it is not, so a backgrounded
 * app makes no requests at all. Restarts when [key] changes.
 */
@Composable
fun PollEffect(key: Any?, baseMs: Long, tick: suspend () -> Unit) {
    val owner = LocalLifecycleOwner.current
    val context = LocalContext.current
    LaunchedEffect(key, owner) {
        owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                tick()
                delay(pollInterval(context, baseMs))
            }
        }
    }
}
