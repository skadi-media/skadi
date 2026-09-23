package com.skadi.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil.compose.AsyncImage
import com.skadi.core.ProfileSetting
import com.skadi.core.SkadiApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Which catalog the Add screen searches. */
enum class AddKind(val label: String) { Movies("Movies"), Tv("TV"), Audiobooks("Audiobooks") }

/** One search hit, normalised across the three catalogs. */
data class MediaHit(
    val key: String,
    val title: String,
    val subtitle: String?,
    val overview: String?,
    val art: String?,
    /** Whether the library already holds this exact provider id. */
    val owned: Boolean,
    val add: suspend (profile: String) -> Boolean,
)

/**
 * Add media (SKADI-T-0601): the web Add page on the phone. Pick a catalog,
 * type a title, get hits with art, choose a quality profile (the daemon's
 * first by default, like the web) and add. Hits the library already holds are
 * marked and cannot be re-added.
 */
@Composable
fun AddMediaScreen(baseUrl: String, token: String?, initial: AddKind, onBack: () -> Unit) {
    val api = remember(baseUrl, token) { SkadiApi(baseUrl, token) }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    var kind by rememberSaveable { mutableStateOf(initial) }
    var query by rememberSaveable { mutableStateOf("") }
    var hits by remember { mutableStateOf<List<MediaHit>>(emptyList()) }
    var searching by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var profiles by remember { mutableStateOf<List<ProfileSetting>>(emptyList()) }
    var profile by rememberSaveable { mutableStateOf<String?>(null) }
    var added by remember { mutableStateOf(setOf<String>()) }
    // Provider ids the library holds, for the "in library" mark.
    var ownedTmdb by remember { mutableStateOf(setOf<Long>()) }
    var ownedTvdb by remember { mutableStateOf(setOf<Long>()) }
    var ownedAsin by remember { mutableStateOf(setOf<String>()) }

    LaunchedEffect(api) {
        runCatching { api.listProfiles() }.onSuccess {
            profiles = it
            if (profile == null) profile = it.firstOrNull()?.id
        }
        runCatching { LibraryCache.movies ?: api.listAllMovies().also { LibraryCache.movies = it } }
            .onSuccess { ms -> ownedTmdb = ms.mapNotNull { it.external_ids?.tmdb }.toSet() }
        runCatching { LibraryCache.series ?: api.listAllSeries().also { LibraryCache.series = it } }
            .onSuccess { ss -> ownedTvdb = ss.mapNotNull { it.external_ids?.tvdb }.toSet() }
        runCatching { api.listBooks() }
            .onSuccess { bs -> ownedAsin = bs.mapNotNull { it.external_ids?.asin ?: it.asin }.toSet() }
    }

    // Debounced search: a keystroke pause of 400 ms runs the lookup.
    LaunchedEffect(query, kind, ownedTmdb, ownedTvdb, ownedAsin) {
        val q = query.trim()
        if (q.length < 2) { hits = emptyList(); return@LaunchedEffect }
        delay(400)
        searching = true
        error = null
        val r = runCatching {
            when (kind) {
                AddKind.Movies -> api.lookupMovies(q).map { h ->
                    MediaHit(
                        key = "tmdb:${h.tmdb_id}", title = h.title, subtitle = h.year?.toString(),
                        overview = h.overview, art = h.poster_url, owned = h.tmdb_id in ownedTmdb,
                        add = { p -> api.addMovie(h.tmdb_id, p) },
                    )
                }
                AddKind.Tv -> api.lookupSeries(q).map { h ->
                    MediaHit(
                        key = "tvdb:${h.tvdb_id}", title = h.title, subtitle = h.year?.toString(),
                        overview = h.overview, art = h.poster_url, owned = h.tvdb_id in ownedTvdb,
                        add = { p -> api.addSeries(h.tvdb_id, p) },
                    )
                }
                AddKind.Audiobooks -> api.searchBooks(q).map { h ->
                    MediaHit(
                        key = "asin:${h.asin}", title = h.title,
                        subtitle = listOfNotNull(h.authors.joinToString(", ").ifEmpty { null }, h.year?.toString()).joinToString(" · "),
                        overview = null, art = h.cover_url, owned = h.asin in ownedAsin,
                        // Audiobooks rank on their own M4B-first ladder; the daemon
                        // ignores the video profile beyond requiring a registered one,
                        // so pass none and let it pick its default.
                        add = { _ -> api.addBook(h.asin, null, search = true) },
                    )
                }
            }
        }
        r.onSuccess { hits = it }.onFailure { error = "Search failed: ${it.message}" }
        searching = false
    }

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = { SkadiTopBar(title = "Add media", onBack = onBack) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { inset ->
        Column(modifier = Modifier.fillMaxSize().padding(inset)) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                AddKind.entries.forEach { k ->
                    FilterChip(selected = k == kind, onClick = { kind = k }, label = { Text(k.label) })
                }
            }
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                singleLine = true,
                placeholder = { Text(when (kind) { AddKind.Audiobooks -> "Title or author"; else -> "Title" }) },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            )
            if (kind != AddKind.Audiobooks) {
                ProfilePicker(profiles = profiles, selected = profile, onSelect = { profile = it })
            }
            when {
                error != null -> Text(error!!, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp))
                searching && hits.isEmpty() -> Loading()
                query.trim().length < 2 -> EmptyState("Type a title to search.")
                hits.isEmpty() -> EmptyState("Nothing found for \"${query.trim()}\".")
                else -> LazyColumn(contentPadding = PaddingValues(bottom = 96.dp)) {
                    items(hits, key = { it.key }) { h ->
                        HitRow(
                            h = h,
                            added = h.key in added,
                            canAdd = profile != null || kind == AddKind.Audiobooks,
                            onAdd = {
                                val p = profile ?: ""
                                scope.launch {
                                    val ok = runCatching { h.add(p) }.getOrDefault(false)
                                    if (ok) {
                                        added = added + h.key
                                        snackbar.showSnackbar("Added ${h.title}")
                                    } else {
                                        snackbar.showSnackbar("Couldn't add ${h.title}")
                                    }
                                }
                            },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun ProfilePicker(profiles: List<ProfileSetting>, selected: String?, onSelect: (String) -> Unit) {
    var open by remember { mutableStateOf(false) }
    val name = profiles.firstOrNull { it.id == selected }?.body?.name
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("Quality", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(8.dp))
        TextButton(onClick = { open = true }, enabled = profiles.isNotEmpty()) {
            Text(name ?: if (profiles.isEmpty()) "…" else "Choose")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            profiles.forEach { p ->
                DropdownMenuItem(text = { Text(p.body.name) }, onClick = { onSelect(p.id); open = false })
            }
        }
    }
}

@Composable
private fun HitRow(h: MediaHit, added: Boolean, canAdd: Boolean, onAdd: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val shape = RoundedCornerShape(6.dp)
        if (h.art != null) {
            AsyncImage(
                model = h.art,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.width(56.dp).aspectRatio(2f / 3f).clip(shape),
            )
        } else {
            ArtPlaceholder(modifier = Modifier.width(56.dp), shape = shape)
        }
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(h.title, style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
            h.subtitle?.takeIf { it.isNotEmpty() }?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            h.overview?.takeIf { it.isNotEmpty() }?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        Spacer(Modifier.width(8.dp))
        when {
            h.owned -> AssistChip(onClick = {}, enabled = false, label = { Text("In library") })
            added -> AssistChip(onClick = {}, enabled = false, label = { Text("Added") })
            else -> Button(onClick = onAdd, enabled = canAdd) { Text("Add") }
        }
    }
}
