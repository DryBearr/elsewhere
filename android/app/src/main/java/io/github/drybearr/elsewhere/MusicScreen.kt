package io.github.drybearr.elsewhere

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.drybearr.elsewhere.playback.Playback
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.elsewhere.CollectionEntry
import uniffi.elsewhere.CollectionSummary
import uniffi.elsewhere.MediaKind
import uniffi.elsewhere.categories
import uniffi.elsewhere.collection
import uniffi.elsewhere.collectionItems
import uniffi.elsewhere.collections

/** Entries asked for per page of a playlist. */
private const val PAGE = 100u

/**
 * A tab of playlists (Music, Videos): its saved playlists; open one to see what is in it
 * and play it. `tab` is the library tab's name.
 */
@Composable
fun MusicScreen(tab: String = "Music") {
    var open by rememberSaveable(tab) { mutableStateOf<Long?>(null) }
    val noun = if (tab == "Music") "song" else "video"
    // Saving a playlist changes what is here: look again when a job ends.
    val jobs by JobsStore.jobs.collectAsState()
    val ended = jobs.values.count { it.status.isFinished }
    val id = open
    if (id == null) {
        Playlists(tab, noun, ended) { open = it }
    } else {
        Playlist(id, noun, ended) { open = null }
    }
}

/** Something loaded off the main thread. */
private sealed interface Loaded<out T> {
    data object Loading : Loaded<Nothing>
    data class Failed(val text: String) : Loaded<Nothing>
    data class Done<T>(val value: T) : Loaded<T>
}

/** A tab's playlists (smart ones have nothing in them yet, so they are not shown). */
private fun playlistsOf(tab: String): List<CollectionSummary> {
    Core.startError?.let { throw IllegalStateException("The core did not start: $it") }
    val found = categories().firstOrNull {
        it.name == tab && (it.media == MediaKind.AUDIO || it.media == MediaKind.VIDEO)
    } ?: return emptyList()
    return collections(found.id).filter { !it.smart }
}

@Composable
private fun Playlists(tab: String, noun: String, ended: Int, onOpen: (Long) -> Unit) {
    var retry by remember { mutableIntStateOf(0) }
    val loaded by produceState<Loaded<List<CollectionSummary>>>(Loaded.Loading, tab, ended, retry) {
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(playlistsOf(tab))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    Column(Modifier.padding(horizontal = 16.dp)) {
        Text(tab, style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(vertical = 12.dp))
        when (val now = loaded) {
            Loaded.Loading -> Unit
            is Loaded.Failed -> Problem(now.text) { retry++ }
            is Loaded.Done -> if (now.value.isEmpty()) {
                Text("No playlists yet. Save one from the Add tab.", color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                LazyColumn {
                    itemsIndexed(now.value, key = { _, playlist -> playlist.id }) { _, playlist ->
                        Column(
                            Modifier.fillMaxWidth().clickable { onOpen(playlist.id) }.padding(vertical = 12.dp),
                        ) {
                            Text(playlist.name, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            Text(
                                count(playlist.entries, noun),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}

/** One playlist: its songs, page by page; tap one to play from there. */
@Composable
private fun Playlist(id: Long, noun: String, ended: Int, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val queue by Playback.state.collectAsState()
    val source by Playback.source.collectAsState()
    val scope = rememberCoroutineScope()
    var retry by remember { mutableIntStateOf(0) }
    // Everything starts over when the playlist may have changed (a job ended) or on Retry.
    val summary by produceState<Loaded<CollectionSummary>>(Loaded.Loading, id, ended, retry) {
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(collection(id))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    val rows = remember(id, ended, retry) { mutableStateListOf<CollectionEntry>() }
    var next by remember(id, ended, retry) { mutableStateOf<String?>(null) }
    var complete by remember(id, ended, retry) { mutableStateOf(false) }
    var loading by remember(id, ended, retry) { mutableStateOf(false) }
    var failed by remember(id, ended, retry) { mutableStateOf<String?>(null) }

    fun more() {
        if (loading || complete) return
        loading = true
        val after = next
        scope.launch {
            try {
                val page = withContext(Dispatchers.IO) { collectionItems(id, after, PAGE) }
                rows += page.entries
                next = page.next
                complete = page.next == null
                failed = null
            } catch (e: Exception) {
                failed = describe(e)
            } finally {
                loading = false
            }
        }
    }
    LaunchedEffect(id, ended, retry) { more() }

    val playingHere = source == id
    val current = queue?.nowPlaying?.itemId
    val skipped = queue?.skipped?.map { it.itemId }?.toSet().orEmpty()

    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            Text(
                when (val now = summary) {
                    is Loaded.Done -> now.value.name
                    else -> ""
                },
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        when (val now = summary) {
            Loaded.Loading -> Unit
            is Loaded.Failed -> Column(Modifier.padding(horizontal = 16.dp)) { Problem(now.text) { retry++ } }
            is Loaded.Done -> LazyColumn(Modifier.padding(horizontal = 16.dp)) {
                item(key = "top") {
                    Row(
                        Modifier.fillMaxWidth().padding(bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(count(now.value.entries, noun), color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Button(
                            onClick = { Playback.playCollection(id, null) },
                            enabled = now.value.entries > 0,
                        ) {
                            Icon(Icons.Filled.PlayArrow, contentDescription = null)
                            Text("Play")
                        }
                    }
                }
                itemsIndexed(rows, key = { _, row -> row.entryId }) { index, row ->
                    val item = row.item
                    val here = playingHere && item.id == current
                    val note = when {
                        item.missing -> "File missing"
                        playingHere && item.id in skipped -> "Could not play"
                        else -> listOfNotNull(item.artist, item.durationMs?.let(::time)).joinToString(" · ")
                    }
                    var menu by remember { mutableStateOf(false) }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(
                            Modifier
                                .weight(1f)
                                .testTag("song-$index")
                                .clickable(enabled = !item.missing) { Playback.playCollection(id, row.entryId) }
                                .alpha(if (item.missing) 0.4f else 1f)
                                .padding(vertical = 10.dp),
                        ) {
                            Text(
                                item.title,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                                fontWeight = if (here) FontWeight.Bold else null,
                                color = if (here) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
                            )
                            if (note.isNotEmpty()) {
                                Text(
                                    note,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.testTag("note-$index"),
                                )
                            }
                        }
                        if (!item.missing) {
                            Box {
                                IconButton(onClick = { menu = true }, modifier = Modifier.testTag("song-menu-$index")) {
                                    Icon(Icons.Filled.MoreVert, contentDescription = "More")
                                }
                                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                    DropdownMenuItem(text = { Text("Play next") }, onClick = {
                                        menu = false
                                        Playback.addNext(item.id)
                                    })
                                    DropdownMenuItem(text = { Text("Add to queue") }, onClick = {
                                        menu = false
                                        Playback.add(item.id)
                                    })
                                }
                            }
                        }
                    }
                    HorizontalDivider()
                    // The last row came into view: the next page.
                    if (index == rows.lastIndex) LaunchedEffect(rows.size) { more() }
                }
                failed?.let { text -> item(key = "failed") { Problem(text) { more() } } }
            }
        }
    }
}

@Composable
private fun Problem(text: String, retry: () -> Unit) {
    Column {
        Text(text, color = MaterialTheme.colorScheme.error)
        TextButton(onClick = retry) { Text("Retry") }
    }
}

private fun count(count: Long, noun: String) = if (count == 1L) "1 $noun" else "$count ${noun}s"

/** 3:07, or 1:02:03 from an hour. */
private fun time(ms: Long): String {
    val seconds = ms / 1000
    val (hours, minutes, rest) = Triple(seconds / 3600, seconds / 60 % 60, seconds % 60)
    return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, rest) else "%d:%02d".format(minutes, rest)
}
