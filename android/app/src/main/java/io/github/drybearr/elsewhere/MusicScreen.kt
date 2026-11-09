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
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
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
import uniffi.elsewhere.copyPlaylist
import uniffi.elsewhere.createPlaylist
import uniffi.elsewhere.deletePlaylist
import uniffi.elsewhere.moveInPlaylist
import uniffi.elsewhere.removeFromPlaylist
import uniffi.elsewhere.renamePlaylist

/** Entries asked for per page of a playlist. */
internal const val PAGE = 100u

/** What a tab shows below its search field. */
internal enum class View { Playlists, Songs, Artists, Albums }

/**
 * A tab of playable things (Music, Videos): its playlists (saved ones and one's own),
 * everything in it, its artists (for videos: channels) and albums, and search. Open a
 * playlist, artist or album to see what is in it and play it. `tab` is the library tab's
 * name.
 */
@Composable
fun MusicScreen(tab: String = "Music") {
    val music = tab == "Music"
    val noun = if (music) "song" else "video"
    val views = if (music) View.entries else listOf(View.Playlists, View.Songs, View.Artists)
    var view by rememberSaveable(tab) { mutableStateOf(View.Playlists) }
    var query by rememberSaveable(tab) { mutableStateOf("") }
    var openPlaylist by rememberSaveable(tab) { mutableStateOf<Long?>(null) }
    var openArtist by rememberSaveable(tab) { mutableStateOf<String?>(null) }
    var openAlbum by rememberSaveable(tab) { mutableStateOf<String?>(null) }
    // Saving a playlist changes what is here: look again when a job ends; and after an edit.
    val jobs by JobsStore.jobs.collectAsState()
    val ended = jobs.values.count { it.status.isFinished }
    var edits by remember { mutableIntStateOf(0) }
    val changes = ended * 100_000 + edits
    val tabId by produceState<Long?>(null, tab) {
        value = withContext(Dispatchers.IO) { runCatching { tabIdOf(tab) }.getOrNull() }
    }
    val context = Edits(tab, noun, tabId, changes) { edits++ }

    val playlist = openPlaylist
    val artist = openArtist
    val album = openAlbum
    when {
        playlist != null -> Playlist(context, playlist) { openPlaylist = null }
        artist != null -> Opened(context, artist, Group.Artist) { openArtist = null }
        album != null -> Opened(context, album, Group.Album) { openAlbum = null }
        else -> Column(Modifier.padding(horizontal = 16.dp)) {
            Text(tab, style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(vertical = 12.dp))
            SearchField(query, noun) { query = it }
            if (query.isNotBlank()) {
                SearchResults(context, query, onPlaylist = { openPlaylist = it }, onArtist = { openArtist = it })
            } else {
                ViewChooser(views, view, music) { view = it }
                when (view) {
                    View.Playlists -> Playlists(context) { openPlaylist = it }
                    View.Songs -> AllItems(context)
                    View.Artists -> Names(context, Group.Artist) { openArtist = it }
                    View.Albums -> Names(context, Group.Album) { openAlbum = it }
                }
            }
        }
    }
}

/** What every part of a tab needs: its name and id, and a way to say "it changed". */
internal class Edits(
    val tab: String,
    val noun: String,
    val tabId: Long?,
    /** Goes up whenever what the tab shows may have changed. */
    val changes: Int,
    val changed: () -> Unit,
)

/** Something loaded off the main thread. */
internal sealed interface Loaded<out T> {
    data object Loading : Loaded<Nothing>
    data class Failed(val text: String) : Loaded<Nothing>
    data class Done<T>(val value: T) : Loaded<T>
}

internal fun tabIdOf(tab: String): Long? {
    Core.startError?.let { throw IllegalStateException("The core did not start: $it") }
    return categories().firstOrNull {
        it.name == tab && (it.media == MediaKind.AUDIO || it.media == MediaKind.VIDEO)
    }?.id
}

/** A tab's playlists (smart ones have nothing in them yet, so they are not shown). */
private fun playlistsOf(tab: String): List<CollectionSummary> {
    val id = tabIdOf(tab) ?: return emptyList()
    return collections(id).filter { !it.smart }
}

@Composable
private fun Playlists(context: Edits, onOpen: (Long) -> Unit) {
    var retry by remember { mutableIntStateOf(0) }
    var naming by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val loaded by produceState<Loaded<List<CollectionSummary>>>(Loaded.Loading, context.tab, context.changes, retry) {
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(playlistsOf(context.tab))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    var problem by remember { mutableStateOf<String?>(null) }
    fun edit(block: () -> Unit) {
        scope.launch {
            problem = withContext(Dispatchers.IO) { runCatching(block).exceptionOrNull()?.let(::describe) }
            context.changed()
        }
    }
    Column {
        TextButton(
            onClick = { naming = true },
            enabled = context.tabId != null,
            modifier = Modifier.testTag("new-playlist"),
        ) {
            Icon(Icons.Filled.Add, contentDescription = null)
            Text("New playlist")
        }
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        when (val now = loaded) {
            Loaded.Loading -> Unit
            is Loaded.Failed -> Problem(now.text) { retry++ }
            is Loaded.Done -> if (now.value.isEmpty()) {
                Text(
                    "No playlists yet. Save one from the Add tab, or make your own.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                LazyColumn {
                    itemsIndexed(now.value, key = { _, playlist -> playlist.id }) { index, playlist ->
                        PlaylistRow(context, index, playlist, onOpen = { onOpen(playlist.id) }, edit = ::edit)
                        HorizontalDivider()
                    }
                }
            }
        }
    }
    if (naming) {
        NameDialog("New playlist", "", onDismiss = { naming = false }) { name ->
            naming = false
            val tabId = context.tabId ?: return@NameDialog
            edit { createPlaylist(tabId, name) }
        }
    }
}

@Composable
private fun PlaylistRow(
    context: Edits,
    index: Int,
    playlist: CollectionSummary,
    onOpen: () -> Unit,
    edit: (() -> Unit) -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var copying by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f).clickable(onClick = onOpen).padding(vertical = 12.dp).testTag("playlist-$index")) {
            Text(playlist.name, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Text(
                count(playlist.entries, context.noun) + if (playlist.source != null) " · saved" else "",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Box {
            IconButton(onClick = { menu = true }, modifier = Modifier.testTag("playlist-menu-$index")) {
                Icon(Icons.Filled.MoreVert, contentDescription = "More")
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("Rename") }, onClick = { menu = false; renaming = true })
                DropdownMenuItem(text = { Text("Copy") }, onClick = { menu = false; copying = true })
                DropdownMenuItem(text = { Text("Delete") }, onClick = { menu = false; deleting = true })
            }
        }
    }
    if (renaming) {
        NameDialog("Rename playlist", playlist.name, onDismiss = { renaming = false }) { name ->
            renaming = false
            edit { renamePlaylist(playlist.id, name) }
        }
    }
    if (copying) {
        NameDialog("Copy playlist", "${playlist.name} (copy)", onDismiss = { copying = false }) { name ->
            copying = false
            edit { copyPlaylist(playlist.id, name) }
        }
    }
    if (deleting) {
        ConfirmDialog(
            "Delete “${playlist.name}”?",
            "Its ${context.noun}s stay in your library.",
            "Delete",
            onDismiss = { deleting = false },
        ) {
            deleting = false
            edit { deletePlaylist(playlist.id) }
        }
    }
}

/**
 * One playlist: what is in it, page by page; tap a row to play from there. One's own
 * playlist can be edited (rows move and come out); a saved one follows its link.
 */
@Composable
private fun Playlist(context: Edits, id: Long, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val queue by Playback.state.collectAsState()
    val source by Playback.source.collectAsState()
    val scope = rememberCoroutineScope()
    var retry by remember { mutableIntStateOf(0) }
    val changes = context.changes
    // Everything starts over when the playlist may have changed (a job, an edit) or on Retry.
    val summary by produceState<Loaded<CollectionSummary>>(Loaded.Loading, id, changes, retry) {
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(collection(id))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    val rows = remember(id, changes, retry) { mutableStateListOf<CollectionEntry>() }
    var next by remember(id, changes, retry) { mutableStateOf<String?>(null) }
    var complete by remember(id, changes, retry) { mutableStateOf(false) }
    var loading by remember(id, changes, retry) { mutableStateOf(false) }
    var failed by remember(id, changes, retry) { mutableStateOf<String?>(null) }
    var problem by remember { mutableStateOf<String?>(null) }

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
    fun edit(block: () -> Unit) {
        scope.launch {
            problem = withContext(Dispatchers.IO) { runCatching(block).exceptionOrNull()?.let(::describe) }
            context.changed()
        }
    }
    LaunchedEffect(id, changes, retry) { more() }

    val playingHere = source == id
    val current = queue?.nowPlaying?.itemId
    val skipped = queue?.skipped?.map { it.itemId }?.toSet().orEmpty()
    val own = when (val now = summary) {
        is Loaded.Done -> now.value.source == null
        else -> false
    }

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
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(horizontal = 16.dp)) }
        when (val now = summary) {
            Loaded.Loading -> Unit
            is Loaded.Failed -> Column(Modifier.padding(horizontal = 16.dp)) { Problem(now.text) { retry++ } }
            is Loaded.Done -> LazyColumn(Modifier.padding(horizontal = 16.dp)) {
                item(key = "top") {
                    Column(Modifier.padding(bottom = 8.dp)) {
                        Row(
                            Modifier.fillMaxWidth(),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween,
                        ) {
                            Text(count(now.value.entries, context.noun), color = MaterialTheme.colorScheme.onSurfaceVariant)
                            Button(
                                onClick = { Playback.playCollection(id, null) },
                                enabled = now.value.entries > 0,
                            ) {
                                Icon(Icons.Filled.PlayArrow, contentDescription = null)
                                Text("Play")
                            }
                        }
                        if (!own) {
                            Text(
                                "Saved from a link: saving it again updates it. Copy it to change it.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
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
                    var picking by remember { mutableStateOf(false) }
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
                        Box {
                            IconButton(onClick = { menu = true }, modifier = Modifier.testTag("song-menu-$index")) {
                                Icon(Icons.Filled.MoreVert, contentDescription = "More")
                            }
                            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                                if (!item.missing) {
                                    DropdownMenuItem(text = { Text("Play next") }, onClick = {
                                        menu = false
                                        Playback.addNext(item.id)
                                    })
                                    DropdownMenuItem(text = { Text("Add to queue") }, onClick = {
                                        menu = false
                                        Playback.add(item.id)
                                    })
                                    DropdownMenuItem(text = { Text("Add to playlist…") }, onClick = {
                                        menu = false
                                        picking = true
                                    })
                                }
                                if (own) {
                                    if (index > 0) {
                                        DropdownMenuItem(text = { Text("Move up") }, onClick = {
                                            menu = false
                                            val before = rows[index - 1].entryId
                                            edit { moveInPlaylist(id, row.entryId, before) }
                                        })
                                    }
                                    if (index < rows.lastIndex || !complete) {
                                        DropdownMenuItem(text = { Text("Move down") }, onClick = {
                                            menu = false
                                            // Before the one after the next (none: to the end).
                                            val before = rows.getOrNull(index + 2)?.entryId
                                            edit { moveInPlaylist(id, row.entryId, before) }
                                        })
                                    }
                                    DropdownMenuItem(text = { Text("Remove from playlist") }, onClick = {
                                        menu = false
                                        edit { removeFromPlaylist(id, listOf(row.entryId)) }
                                    })
                                }
                            }
                        }
                    }
                    if (picking) PlaylistPicker(context, listOf(item.id), onDone = { picking = false }, edit = ::edit)
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
internal fun Problem(text: String, retry: () -> Unit) {
    Column {
        Text(text, color = MaterialTheme.colorScheme.error)
        TextButton(onClick = retry) { Text("Retry") }
    }
}

internal fun count(count: Long, noun: String) = if (count == 1L) "1 $noun" else "$count ${noun}s"

/** 3:07, or 1:02:03 from an hour. */
internal fun time(ms: Long): String {
    val seconds = ms / 1000
    val (hours, minutes, rest) = Triple(seconds / 3600, seconds / 60 % 60, seconds % 60)
    return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, rest) else "%d:%02d".format(minutes, rest)
}
