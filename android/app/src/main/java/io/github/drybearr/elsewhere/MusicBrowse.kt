package io.github.drybearr.elsewhere

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.elsewhere.CollectionSummary
import uniffi.elsewhere.ItemOrder
import uniffi.elsewhere.ItemPage
import uniffi.elsewhere.ItemSummary
import uniffi.elsewhere.Named
import uniffi.elsewhere.SearchResult
import uniffi.elsewhere.addToPlaylist
import uniffi.elsewhere.albumItems
import uniffi.elsewhere.albums
import uniffi.elsewhere.artistItems
import uniffi.elsewhere.artists
import uniffi.elsewhere.collections
import uniffi.elsewhere.createPlaylist
import uniffi.elsewhere.search
import uniffi.elsewhere.tabItems

/** What an opened name is: an artist (for videos: a channel) or an album. */
internal enum class Group { Artist, Album }

@Composable
internal fun SearchField(query: String, noun: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value = query,
        onValueChange = onChange,
        singleLine = true,
        placeholder = { Text("Search ${noun}s, playlists, artists") },
        trailingIcon = {
            if (query.isNotEmpty()) {
                IconButton(onClick = { onChange("") }) { Icon(Icons.Filled.Clear, contentDescription = "Clear search") }
            }
        },
        modifier = Modifier.fillMaxWidth().testTag("search"),
    )
}

@Composable
internal fun ViewChooser(views: List<View>, view: View, music: Boolean, onChoose: (View) -> Unit) {
    TabRow(selectedTabIndex = views.indexOf(view).coerceAtLeast(0), modifier = Modifier.padding(vertical = 8.dp)) {
        views.forEach { choice ->
            val label = when (choice) {
                View.Playlists -> "Playlists"
                View.Songs -> if (music) "Songs" else "Videos"
                View.Artists -> if (music) "Artists" else "Channels"
                View.Albums -> "Albums"
            }
            Tab(
                selected = choice == view,
                onClick = { onChoose(choice) },
                text = { Text(label, maxLines = 1) },
                modifier = Modifier.testTag("view-${choice.name}"),
            )
        }
    }
}

/** Everything the tab holds, A to Z, newest first, or last played first. */
@Composable
internal fun AllItems(context: Edits) {
    val tabId = context.tabId ?: return
    var order by rememberSaveable { mutableStateOf(ItemOrder.TITLE) }
    Column {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            for ((choice, label) in listOf(ItemOrder.TITLE to "A to Z", ItemOrder.ADDED to "Newest", ItemOrder.PLAYED to "Played")) {
                TextButton(onClick = { order = choice }, modifier = Modifier.testTag("order-${choice.name}")) {
                    Text(label, fontWeight = if (choice == order) FontWeight.Bold else null)
                }
            }
        }
        PagedItems(context, key = "all-$order", empty = "Nothing here yet. Save something from the Add tab.") { after ->
            tabItems(tabId, order, after, PAGE)
        }
    }
}

/** The tab's artists or albums; tap one to see its items. */
@Composable
internal fun Names(context: Edits, group: Group, onOpen: (String) -> Unit) {
    val tabId = context.tabId ?: return
    var retry by remember { mutableIntStateOf(0) }
    val loaded by produceState<Loaded<List<Named>>>(Loaded.Loading, tabId, group, context.changes, retry) {
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(if (group == Group.Artist) artists(tabId) else albums(tabId))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    when (val now = loaded) {
        Loaded.Loading -> Unit
        is Loaded.Failed -> Problem(now.text) { retry++ }
        is Loaded.Done -> if (now.value.isEmpty()) {
            Text(
                if (group == Group.Artist) "No artists yet." else "No albums yet.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            LazyColumn {
                itemsIndexed(now.value, key = { _, named -> named.name }) { index, named ->
                    NameRow(context, index, named) { onOpen(named.name) }
                    HorizontalDivider()
                }
            }
        }
    }
}

@Composable
private fun NameRow(context: Edits, index: Int, named: Named, onOpen: () -> Unit) {
    Column(Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(vertical = 12.dp).testTag("name-$index")) {
        Text(named.name, maxLines = 2, overflow = TextOverflow.Ellipsis)
        Text(count(named.items, context.noun), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** An artist's or album's items, with Play. */
@Composable
internal fun Opened(context: Edits, name: String, group: Group, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val tabId = context.tabId ?: return
    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back") }
            Text(name, style = MaterialTheme.typography.titleLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Column(Modifier.padding(horizontal = 16.dp)) {
            PagedItems(context, key = "$group-$name", empty = "Nothing here any more.", play = true) { after ->
                if (group == Group.Artist) artistItems(tabId, name, after, PAGE) else albumItems(tabId, name, after, PAGE)
            }
        }
    }
}

/**
 * Items a page at a time; tap one to play the ones shown from there. With `play`, a Play
 * button plays them all from the top.
 */
@Composable
private fun PagedItems(
    context: Edits,
    key: String,
    empty: String,
    play: Boolean = false,
    load: (String?) -> ItemPage,
) {
    val scope = rememberCoroutineScope()
    var retry by remember { mutableIntStateOf(0) }
    val rows = remember(key, context.changes, retry) { mutableStateListOf<ItemSummary>() }
    var next by remember(key, context.changes, retry) { mutableStateOf<String?>(null) }
    var complete by remember(key, context.changes, retry) { mutableStateOf(false) }
    var loading by remember(key, context.changes, retry) { mutableStateOf(false) }
    var failed by remember(key, context.changes, retry) { mutableStateOf<String?>(null) }
    var loadedOnce by remember(key, context.changes, retry) { mutableStateOf(false) }
    fun more() {
        if (loading || complete) return
        loading = true
        val after = next
        scope.launch {
            try {
                val page = withContext(Dispatchers.IO) { load(after) }
                rows += page.items
                next = page.next
                complete = page.next == null
                failed = null
            } catch (e: Exception) {
                failed = describe(e)
            } finally {
                loading = false
                loadedOnce = true
            }
        }
    }
    LaunchedEffect(key, context.changes, retry) { more() }
    LazyColumn {
        if (play && rows.isNotEmpty()) {
            item(key = "play") {
                Row(Modifier.fillMaxWidth().padding(bottom = 8.dp), horizontalArrangement = Arrangement.End) {
                    Button(onClick = { Playback.playItems(rows.map { it.id }, 0) }, modifier = Modifier.testTag("play-all")) {
                        Icon(Icons.Filled.PlayArrow, contentDescription = null)
                        Text("Play")
                    }
                }
            }
        }
        if (loadedOnce && rows.isEmpty() && failed == null) {
            item(key = "empty") { Text(empty, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        itemRows(context, rows) { index -> if (index == rows.lastIndex) more() }
        failed?.let { text -> item(key = "failed") { Problem(text) { more() } } }
    }
}

/** Rows of items: tap one to play the rows from it; its menu queues it or adds it to a playlist. */
private fun LazyListScope.itemRows(context: Edits, rows: List<ItemSummary>, prefix: String = "item", shown: (Int) -> Unit = {}) {
    itemsIndexed(rows, key = { index, item -> "$prefix-$index-${item.id}" }) { index, item ->
        ItemRow(context, index, item, prefix) { Playback.playItems(rows.map { it.id }, index) }
        HorizontalDivider()
        LaunchedEffect(index, rows.size) { shown(index) }
    }
}

@Composable
private fun ItemRow(context: Edits, index: Int, item: ItemSummary, prefix: String, onPlay: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    var picking by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(
            Modifier
                .weight(1f)
                .testTag("$prefix-$index")
                .clickable(enabled = !item.missing, onClick = onPlay)
                .alpha(if (item.missing) 0.4f else 1f)
                .padding(vertical = 10.dp),
        ) {
            Text(item.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
            val note = if (item.missing) {
                "File missing"
            } else {
                listOfNotNull(item.artist, item.durationMs?.let(::time)).joinToString(" · ")
            }
            if (note.isNotEmpty()) {
                Text(note, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (!item.missing) {
            Box {
                IconButton(onClick = { menu = true }, modifier = Modifier.testTag("$prefix-menu-$index")) {
                    Icon(Icons.Filled.MoreVert, contentDescription = "More")
                }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(text = { Text("Play next") }, onClick = { menu = false; Playback.addNext(item.id) })
                    DropdownMenuItem(text = { Text("Add to queue") }, onClick = { menu = false; Playback.add(item.id) })
                    DropdownMenuItem(text = { Text("Add to playlist…") }, onClick = { menu = false; picking = true })
                }
            }
        }
    }
    if (picking) {
        PlaylistPicker(context, listOf(item.id), onDone = { picking = false }) { block ->
            scope.launch {
                withContext(Dispatchers.IO) { runCatching(block) }
                context.changed()
            }
        }
    }
}

/** What a search finds as you type: items, playlists, artists. */
@Composable
internal fun SearchResults(context: Edits, query: String, onPlaylist: (Long) -> Unit, onArtist: (String) -> Unit) {
    val tabId = context.tabId ?: return
    val found by produceState<Loaded<SearchResult>>(Loaded.Loading, tabId, query, context.changes) {
        // Typing: look once it pauses.
        delay(250)
        value = withContext(Dispatchers.IO) {
            try {
                Loaded.Done(search(tabId, query))
            } catch (e: Exception) {
                Loaded.Failed(describe(e))
            }
        }
    }
    when (val now = found) {
        Loaded.Loading -> Unit
        is Loaded.Failed -> Text(now.text, color = MaterialTheme.colorScheme.error)
        is Loaded.Done -> {
            val result = now.value
            if (result.items.isEmpty() && result.playlists.isEmpty() && result.artists.isEmpty()) {
                Text("Nothing found.", color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 12.dp))
            } else {
                LazyColumn(Modifier.testTag("results")) {
                    if (result.items.isNotEmpty()) {
                        item(key = "items") { Section("${context.noun.replaceFirstChar { it.uppercase() }}s") }
                        itemRows(context, result.items, prefix = "found")
                    }
                    if (result.playlists.isNotEmpty()) {
                        item(key = "playlists") { Section("Playlists") }
                        itemsIndexed(result.playlists, key = { _, playlist -> "playlist-${playlist.id}" }) { index, playlist ->
                            Column(
                                Modifier.fillMaxWidth().clickable { onPlaylist(playlist.id) }.padding(vertical = 12.dp)
                                    .testTag("found-playlist-$index"),
                            ) {
                                Text(playlist.name, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                Text(count(playlist.entries, context.noun), style = MaterialTheme.typography.bodySmall)
                            }
                            HorizontalDivider()
                        }
                    }
                    if (result.artists.isNotEmpty()) {
                        item(key = "artists") { Section(if (context.noun == "song") "Artists" else "Channels") }
                        itemsIndexed(result.artists, key = { _, named -> "artist-${named.name}" }) { index, named ->
                            Column(
                                Modifier.fillMaxWidth().clickable { onArtist(named.name) }.padding(vertical = 12.dp)
                                    .testTag("found-artist-$index"),
                            ) {
                                Text(named.name, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                Text(count(named.items, context.noun), style = MaterialTheme.typography.bodySmall)
                            }
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Section(title: String) {
    Text(title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(top = 16.dp, bottom = 4.dp))
}

/** Asks for a name (a new playlist, a new name). */
@Composable
internal fun NameDialog(title: String, initial: String, onDismiss: () -> Unit, onDone: (String) -> Unit) {
    var name by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                singleLine = true,
                label = { Text("Name") },
                modifier = Modifier.testTag("name-field"),
            )
        },
        confirmButton = {
            TextButton(onClick = { onDone(name.trim()) }, enabled = name.isNotBlank(), modifier = Modifier.testTag("name-ok")) {
                Text("OK")
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
internal fun ConfirmDialog(title: String, text: String, action: String, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = { TextButton(onClick = onConfirm, modifier = Modifier.testTag("confirm")) { Text(action) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Picks one of one's own playlists of the tab (or makes a new one) and adds `items` to it. */
@Composable
internal fun PlaylistPicker(context: Edits, items: List<Long>, onDone: () -> Unit, edit: (() -> Unit) -> Unit) {
    val tabId = context.tabId ?: return onDone()
    var naming by remember { mutableStateOf(false) }
    val own by produceState<List<CollectionSummary>?>(null, tabId, context.changes) {
        value = withContext(Dispatchers.IO) {
            runCatching { collections(tabId).filter { it.source == null && !it.smart } }.getOrDefault(emptyList())
        }
    }
    if (naming) {
        NameDialog("New playlist", "", onDismiss = onDone) { name ->
            onDone()
            edit {
                val made = createPlaylist(tabId, name)
                addToPlaylist(made, items, null)
            }
        }
        return
    }
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Add to playlist") },
        text = {
            LazyColumn(Modifier.heightIn(max = 360.dp)) {
                item(key = "new") {
                    TextButton(onClick = { naming = true }, modifier = Modifier.testTag("pick-new")) { Text("New playlist…") }
                }
                itemsIndexed(own.orEmpty(), key = { _, playlist -> playlist.id }) { index, playlist ->
                    Text(
                        playlist.name,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable {
                                onDone()
                                edit { addToPlaylist(playlist.id, items, null) }
                            }
                            .padding(vertical = 12.dp)
                            .testTag("pick-$index"),
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDone) { Text("Cancel") } },
    )
}
