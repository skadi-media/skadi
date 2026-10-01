package com.skadi.app

import androidx.compose.ui.unit.sp
import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import kotlinx.coroutines.launch
import androidx.lifecycle.lifecycleScope
import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Headphones
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Movie
import androidx.compose.material.icons.filled.Tv
import androidx.compose.material.icons.outlined.Headphones
import androidx.compose.material.icons.outlined.Home
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.Tv
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.produceState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import com.skadi.core.SkadiApi
import com.skadi.core.VideoProgress
import com.skadi.core.WatchRecord

class MainActivity : ComponentActivity() {
    // API 33+: the media notification (lockscreen controls!) is suppressed
    // until POST_NOTIFICATIONS is granted (review pass 2, A5).
    private val askNotif =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            askNotif.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        pairFromLink(intent)
        // PiP actions (SKADI-T-0665): play/pause and ±10 s, sent by the system
        // as broadcasts to this app only.
        androidx.core.content.ContextCompat.registerReceiver(
            this,
            pipReceiver,
            android.content.IntentFilter(Pip.ACTION),
            androidx.core.content.ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        setContent { SkadiTheme { Root() } }
    }

    private val pipReceiver = object : android.content.BroadcastReceiver() {
        override fun onReceive(context: android.content.Context, intent: Intent) {
            Pip.handle(intent.getIntExtra(Pip.EXTRA, 0))
            Pip.update(this@MainActivity)
        }
    }

    override fun onDestroy() {
        runCatching { unregisterReceiver(pipReceiver) }
        super.onDestroy()
    }

    /** Leaving while a video is on screen shrinks it into a PiP window (SKADI-T-0665). */
    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        if (Pip.video != null && !isInPictureInPictureMode) {
            runCatching { enterPictureInPictureMode(Pip.params(this)) }
        }
    }

    override fun onPictureInPictureModeChanged(
        isInPictureInPictureMode: Boolean,
        newConfig: android.content.res.Configuration,
    ) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig)
        Pip.inPip = isInPictureInPictureMode
        // Leaving PiP with the activity not back in front means the window was
        // closed, not expanded: stop the film and keep its place.
        if (!isInPictureInPictureMode &&
            !lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED)
        ) {
            Pip.video?.onDismissed?.invoke()
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        pairFromLink(intent)
    }

    /**
     * A tapped `skadi://pair?host=&token=` link (SKADI-T-0616): the same payload
     * as the pairing QR, delivered by any message instead of a camera. Saving
     * the server re-pairs the phone as that member; `Root` observes the store
     * and redraws. Links without a host are ignored.
     */
    private fun pairFromLink(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme != "skadi" || uri.host != "pair") return
        val host = uri.getQueryParameter("host")?.trim().orEmpty()
        val token = uri.getQueryParameter("token")?.trim().orEmpty()
        if (host.isEmpty()) return
        val baseUrl = if (host.contains("://")) host else "http://$host"
        intent.data = null
        lifecycleScope.launch {
            runCatching { Settings.save(applicationContext, baseUrl, token) }
                .onSuccess { android.util.Log.i("skadi", "paired from link: $baseUrl") }
                .onFailure { android.util.Log.w("skadi", "pair from link failed: $it") }
        }
    }
}

@Composable
fun SkadiTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        // One source of truth for the app palette (SKADI-T-0356) — screens read
        // MaterialTheme.colorScheme.* instead of scattered Color(0xFF…) literals.
        //
        // `primary` is for *actions* (play, the selected tab, the active rail
        // letter). Titles are `onBackground`: a heading in the accent colour
        // competes with the buttons next to it, which is part of what made the
        // old headers feel busy (SKADI-T-0577).
        colorScheme = darkColorScheme(
            primary = Color(0xFF7FB2FF),
            onPrimary = Color(0xFF0E1116),
            secondary = Color(0xFF6FBF87), // "up next" / success green
            onSecondary = Color(0xFF0E1116),
            tertiary = Color(0xFFE0B060), // offline / warning amber
            error = Color(0xFFF06464), // destructive (unpair)
            background = Color(0xFF0E1116),
            onBackground = Color(0xFFE2E8F0),
            surface = Color(0xFF12161C),
            onSurface = Color(0xFFE2E8F0),
            surfaceVariant = Color(0xFF1E252E), // chips, placeholders, bars
            onSurfaceVariant = Color(0xFF8A93A2), // muted/secondary text
            surfaceContainer = Color(0xFF161B22), // nav bar, mini-player, sheets
            secondaryContainer = Color(0xFF243247), // selected tab / chip pill
            onSecondaryContainer = Color(0xFFDDE8FF),
        ),
        content = content,
    )
}

/**
 * The three libraries, as tabs (SKADI-T-0577).
 *
 * Audiobooks used to *be* the app, with Movies and TV as text buttons in its
 * header that opened sub-screens ending in "Done". Three equal libraries were
 * presented as one library and two side doors. A bottom navigation bar is what
 * Material specifies for three to five top-level destinations, and it is what
 * every other media app on the phone does, so the model needs no learning.
 */
private enum class Tab(val label: String, val icon: ImageVector, val outline: ImageVector) {
    // Home first and default (SKADI-T-0578): the app opens on what you were in
    // the middle of, not on a list of everything.
    Home("Home", Icons.Filled.Home, Icons.Outlined.Home),
    // "Books", not "Audiobooks": six items share the bar (SKADI-T-0608) and the
    // long label clipped to "Audiobook".
    Audiobooks("Books", Icons.Filled.Headphones, Icons.Outlined.Headphones),
    Movies("Movies", Icons.Filled.Movie, Icons.Outlined.Movie),
    Tv("TV", Icons.Filled.Tv, Icons.Outlined.Tv),
}

@Composable
fun Root() {
    val context = LocalContext.current
    // Three states, not two: "not read yet" used to be indistinguishable from
    // "not paired", so every launch flashed the pairing screen and started an
    // mDNS search before DataStore had answered.
    val server by produceState<Loaded?>(initialValue = null) {
        Settings.server(context).collect {
            android.util.Log.i("skadi", "server store: ${it?.baseUrl ?: "none"}")
            value = Loaded(it)
        }
    }

    // Who holds the phone (SKADI-T-0614): the cached role draws the right bar
    // on a cold start; `/me` refreshes it on every foreground (and every 15
    // minutes while open) so a role change on the server lands without a
    // re-pair. No cached role means a pairing older than roles: the operator.
    val cachedRole by Settings.role(context).collectAsState(initial = null)
    val role = cachedRole ?: "admin"

    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        when (val s = server?.server) {
            null -> if (server != null) PairingScreen(onPaired = {})
            else -> {
                val me = remember(s.baseUrl, s.token) { SkadiApi(s.baseUrl, s.token) }
                PollEffect(key = s.token, baseMs = 15 * 60 * 1000L) {
                    runCatching { me.me() }
                        .onSuccess { Settings.saveRole(context, it.role) }
                        .onFailure {
                            // Revoked server-side (or the password changed):
                            // drop the credential, which puts `Root` back on
                            // the login screen. Any other failure is the
                            // network and must NOT sign anyone out
                            // (SKADI-T-0622).
                            if (it is com.skadi.core.SessionExpiredException) {
                                android.util.Log.i("skadi", "signed out by the server")
                                Settings.clear(context)
                            } else {
                                android.util.Log.w("skadi", "/me failed: $it")
                            }
                        }
                }
                CompositionLocalProvider(LocalRole provides role) {
                    Paired(baseUrl = s.baseUrl, token = s.token)
                }
            }
        }
    }
}

private class Loaded(val server: Settings.Server?)

@Composable
private fun Paired(baseUrl: String, token: String) {
    val context = LocalContext.current
    val admin = isAdmin()
    // Adding media is a contributor's whole point, so the Add item is theirs
    // too (SKADI-T-0625).
    val canAdd = canContribute()
    // Unpair (Books → ⋮ → Unpair) used to be wired to nothing (SKADI-T-0616):
    // the confirm dialog closed and the phone stayed paired, so a wrong pairing
    // meant a reinstall. Clearing the store sends `Root` back to the pairing
    // screen; a tapped pairing link re-pairs without that, by overwriting.
    val unpairScope = rememberCoroutineScope()
    val unpair: () -> Unit = { unpairScope.launch { Settings.clear(context.applicationContext) } }
    // The same four for everyone now (SKADI-T-0627): Downloads was a fifth tab
    // that only the operator saw, which pushed the bar to six items with Add
    // and clipped the labels. It is a status row on Home instead — the place
    // that already answers "what is happening right now".
    val tabs = Tab.entries
    var tab by rememberSaveable { mutableStateOf(Tab.Home) }
    var playingFid by rememberSaveable { mutableStateOf<String?>(null) }
    var showStorage by rememberSaveable { mutableStateOf(false) }
    // Add-media overlay (SKADI-T-0601), keyed by the kind the current tab implies.
    var showAdd by rememberSaveable { mutableStateOf<String?>(null) }
    var showWanted by rememberSaveable { mutableStateOf(false) }
    var showDownloads by rememberSaveable { mutableStateOf(false) }
    var changingPassword by rememberSaveable { mutableStateOf(false) }
    var showServer by rememberSaveable { mutableStateOf(false) }
    // A show another tab asked the TV tab to open (SKADI-T-0608).
    var tvRequest by remember { mutableStateOf<Pair<String, Int>?>(null) }
    val openSeries: (String, Int?) -> Unit = { sid, season ->
        showWanted = false
        tvRequest = sid to (season ?: 1)
        tab = Tab.Tv
    }
    // Video is a separate destination, not a mode of the audio player
    // (SKADI-T-0575): it wants a surface, landscape and its own ExoPlayer, none
    // of which the audiobook MediaSession does. (url, title, progressKey) — a
    // Triple because `rememberSaveable` stores it directly.
    var playingVideo by rememberSaveable { mutableStateOf<Triple<String, String, String>?>(null) }
    // Created HERE, above the overlays' early returns: it used to be created
    // below them, so while a video played the holder itself left composition
    // and every tab's state with it — back from the player landed on the show
    // list instead of the episode you came from (SKADI-T-0586).
    val tabState = rememberSaveableStateHolder()

    // What follows the episode being watched, resolved from the series once
    // per video (SKADI-T-0585): the Next button in the player, and where the
    // player goes when the file ends. Movies resolve to nothing.
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    var nextUp by remember { mutableStateOf<NextUp?>(null) }
    LaunchedEffect(playingVideo?.third) {
        nextUp = null
        val key = playingVideo?.third ?: return@LaunchedEffect
        nextUp = resolveNext(api, key)
    }

    // Full-screen overlays, innermost first. Each owns its own BackHandler so
    // the system back gesture closes it instead of leaving the app — before
    // this there was no BackHandler anywhere, and back from Movies quit
    // (SKADI-T-0577).
    playingVideo?.let { (url, title, progressKey) ->
        BackHandler { playingVideo = null }
        // Keyed by url: the player is `remember`ed inside, so advancing to the
        // next episode must be a fresh instance, not the old player with a
        // new title over it.
        androidx.compose.runtime.key(url) {
            VideoPlayerScreen(
                url = url,
                title = title,
                progressKey = progressKey,
                onBack = { playingVideo = null },
                nextLabel = nextUp?.label,
                onNext = nextUp?.let { n ->
                    {
                        VideoProgress(context).start(n.record)
                        playingVideo = n.target
                    }
                },
            )
        }
        return
    }
    playingFid?.let { fid ->
        BackHandler { playingFid = null }
        PlayerScreen(fid = fid, onBack = { playingFid = null })
        return
    }
    if (showStorage) {
        BackHandler { showStorage = false }
        StorageScreen(onBack = { showStorage = false })
        return
    }
    if (showServer) {
        BackHandler { showServer = false }
        ServerScreen(baseUrl = baseUrl, token = token, onBack = { showServer = false })
        return
    }
    if (showDownloads) {
        BackHandler { showDownloads = false }
        DownloadsScreen(
            baseUrl = baseUrl,
            token = token,
            onWanted = { showWanted = true },
            onServer = { showServer = true },
        )
        return
    }
    if (changingPassword) {
        PasswordDialog(
            api = remember(baseUrl, token) { SkadiApi(baseUrl, token) },
            onDismiss = { changingPassword = false },
        )
    }
    if (showWanted) {
        BackHandler { showWanted = false }
        WantedScreen(baseUrl = baseUrl, token = token, onBack = { showWanted = false }, onOpenSeries = openSeries)
        return
    }
    showAdd?.let { kind ->
        BackHandler { showAdd = null }
        AddMediaScreen(
            baseUrl = baseUrl,
            token = token,
            initial = AddKind.valueOf(kind),
            onBack = { showAdd = null },
        )
        return
    }

    // Each tab's rememberSaveable state (query, open series, scroll position)
    // survives switching away and back. Without the holder a tab is rebuilt
    // from scratch every time it is selected, which is the "where was I"
    // problem tabs exist to avoid.
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        bottomBar = {
            Column {
                // Docked above the tabs, full width: the now-playing strip is
                // part of the app's frame, not a card floating over a list.
                MiniPlayerBar(onOpen = { playingFid = it })
                NavigationBar(containerColor = MaterialTheme.colorScheme.surfaceContainer) {
                    tabs.forEach { t ->
                        val selected = t == tab
                        NavigationBarItem(
                            selected = selected,
                            onClick = { tab = t },
                            icon = { Icon(if (selected) t.icon else t.outline, contentDescription = null) },
                            // Five tabs: a label must not wrap ("Audiobook / s").
                            label = { Text(t.label, maxLines = 1, softWrap = false, style = MaterialTheme.typography.labelSmall, fontSize = 10.sp) },
                            colors = NavigationBarItemDefaults.colors(
                                selectedIconColor = MaterialTheme.colorScheme.onSecondaryContainer,
                                selectedTextColor = MaterialTheme.colorScheme.onBackground,
                                indicatorColor = MaterialTheme.colorScheme.secondaryContainer,
                                unselectedIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
                                unselectedTextColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            ),
                        )
                    }
                    // Add media lives in the bar, not over the content: the
                    // floating button covered the last poster of every wall
                    // (SKADI-T-0608). Kind is preset from the tab you were on.
                    // Operator and contributor (SKADI-T-0614, SKADI-T-0625).
                    if (canAdd) NavigationBarItem(
                        selected = false,
                        onClick = {
                            showAdd = when (tab) {
                                Tab.Audiobooks -> AddKind.Audiobooks
                                Tab.Tv -> AddKind.Tv
                                else -> AddKind.Movies
                            }.name
                        },
                        icon = { Icon(Icons.Filled.Add, contentDescription = "Add media") },
                        label = { Text("Add", maxLines = 1, softWrap = false, style = MaterialTheme.typography.labelSmall, fontSize = 10.sp) },
                        colors = NavigationBarItemDefaults.colors(
                            unselectedIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
                            unselectedTextColor = MaterialTheme.colorScheme.onSurfaceVariant,
                        ),
                    )
                }
            }
        },
    ) { inset ->
        Box(modifier = Modifier.fillMaxSize().padding(inset)) {
            tabState.SaveableStateProvider(tab.name) {
                when (tab) {
                    Tab.Home -> HomeScreen(
                        baseUrl = baseUrl,
                        token = token,
                        isAdmin = admin,
                        onOpenDownloads = { showDownloads = true },
                        onManageStorage = { showStorage = true },
                        onChangePassword = { changingPassword = true },
                        onSignOut = unpair,
                        onPlayVideo = { url, title, key -> playingVideo = Triple(url, title, key) },
                        onPlayBook = { playingFid = it },
                        onOpenSeries = { sid, season -> openSeries(sid, season) },
                    )
                    Tab.Audiobooks -> LibraryScreen(
                        baseUrl = baseUrl,
                        token = token,
                        onPlay = { playingFid = it },
                    )
                    Tab.Movies -> MoviesScreen(
                        baseUrl = baseUrl,
                        token = token,
                        onPlay = { url, title, key -> playingVideo = Triple(url, title, key) },
                    )
                    Tab.Tv -> TvScreen(
                        baseUrl = baseUrl,
                        token = token,
                        onPlay = { url, title, key -> playingVideo = Triple(url, title, key) },
                        request = tvRequest,
                        onRequestConsumed = { tvRequest = null },
                    )
                }
            }
        }
    }
}

/** The episode after the one being watched: what to play, and how Home should record it. */
private class NextUp(val target: Triple<String, String, String>, val label: String, val record: WatchRecord)

/**
 * Resolve the next playable episode for a video progress key, or `null` for
 * a movie, the last episode, or a series that cannot be fetched. Same rule as
 * Home's Up next: broadcast order, specials skipped, only what is on disk.
 */
private suspend fun resolveNext(api: SkadiApi, key: String): NextUp? {
    val parts = key.split(':')
    if (parts.size != 3 || parts[0] != "episode") return null
    val (sid, eid) = parts[1] to parts[2]
    val series = LibraryCache.seriesDetail[sid]
        ?: runCatching { api.series(sid) }.getOrNull()?.also { LibraryCache.seriesDetail[sid] = it }
        ?: return null
    val current = series.episodes.firstOrNull { it.id == eid } ?: return null
    val next = nextEpisode(series.episodes, current.season, current.number) ?: return null
    val nextKey = "episode:$sid:${next.id}"
    return NextUp(
        target = Triple(api.episodeVideoUrl(sid, next.id), "${series.title} · ${next.code}", nextKey),
        label = next.code,
        record = WatchRecord(
            key = nextKey, kind = "episode", parentId = sid, itemId = next.id,
            title = series.title,
            subtitle = listOfNotNull(next.code, next.title).joinToString(" · "),
            posterUrl = series.poster_url, season = next.season, number = next.number,
        ),
    )
}
