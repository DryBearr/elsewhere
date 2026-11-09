package io.github.drybearr.elsewhere.playback

import android.os.SystemClock
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.elsewhere.ItemSummary
import uniffi.elsewhere.PlayerStatus
import uniffi.elsewhere.QueueState
import uniffi.elsewhere.RepeatMode
import uniffi.elsewhere.collection
import uniffi.elsewhere.items

/** Speeds the speed menu offers. */
private val SPEEDS = listOf(0.5, 0.75, 1.0, 1.25, 1.5, 2.0)

/** Queue rows read from the library at once. */
private const val ROWS = 100

/**
 * The player, full screen: the cover, what plays, a position slider, the controls (with
 * repeat, shuffle and speed) and the queue. Opened from the player bar; Back or the arrow
 * closes it, and it closes itself when the queue empties. Playback goes on either way.
 */
@Composable
fun PlayerScreen(onClose: () -> Unit) {
    BackHandler(onBack = onClose)
    val state by Playback.state.collectAsState()
    val queue = state
    val current = queue?.current
    if (queue == null || current == null || current >= queue.entryCount) {
        LaunchedEffect(Unit) { onClose() }
        return
    }
    DisposableEffect(Unit) {
        Playback.wantEntries(true)
        onDispose { Playback.wantEntries(false) }
    }
    val entries by Playback.entries.collectAsState()
    // Rows by item id (null: no longer in the library), read as they come into view.
    val rows = remember { mutableStateMapOf<Long, ItemSummary?>() }
    val listed = entries?.takeIf { it.revision == queue.entriesRevision }?.entries.orEmpty()
    val scroll = rememberLazyListState()
    LaunchedEffect(listed) {
        snapshotFlow { scroll.layoutInfo.visibleItemsInfo.map { it.index } }.collect { visible ->
            // The list's first two items are the player and the queue's title.
            val chunks = visible.map { it - 2 }.filter { it in listed.indices }.map { it / ROWS }.distinct()
            for (chunk in chunks) {
                val ids = listed.subList(chunk * ROWS, minOf(listed.size, chunk * ROWS + ROWS))
                    .map { it.itemId }.filter { it !in rows }.distinct()
                if (ids.isEmpty()) continue
                val found = withContext(Dispatchers.IO) { runCatching { items(ids) }.getOrNull() } ?: continue
                ids.zip(found).forEach { (id, row) -> rows[id] = row }
            }
        }
    }

    Surface(Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize().testTag("player-list"), state = scroll) {
            item(key = "player") { Player(queue, onClose) }
            item(key = "next") {
                Text(
                    "Queue · ${queue.entryCount}",
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
                )
            }
            itemsIndexed(listed, key = { _, entry -> entry.key }) { index, entry ->
                QueueRow(
                    index = index,
                    row = rows[entry.itemId],
                    known = entry.itemId in rows,
                    playing = entry.key == queue.currentKey,
                    onPlay = { Playback.jump(entry.key) },
                    onPlayNext = { Playback.playNext(entry.key) },
                    onMove = { to -> Playback.moveEntry(entry.key, to) },
                    onRemove = { Playback.remove(entry.key) },
                    last = index == listed.lastIndex,
                )
                HorizontalDivider(Modifier.padding(start = 16.dp))
            }
        }
    }
}

/** The top of the screen: cover, titles, position, controls. */
@Composable
private fun Player(queue: QueueState, onClose: () -> Unit) {
    val playback = queue.playback
    val now = queue.nowPlaying
    val message by Playback.message.collectAsState()
    val source by Playback.source.collectAsState()
    val playlist by produceState<String?>(null, source) {
        value = source?.let { id -> withContext(Dispatchers.IO) { runCatching { collection(id).name }.getOrNull() } }
    }
    val cover by produceState<ImageBitmap?>(null, now?.cover) {
        value = now?.cover?.let { path ->
            withContext(Dispatchers.IO) { Covers.load(path, 1024)?.asImageBitmap() }
        }
    }
    val failed = queue.finished && playback.status == PlayerStatus.FAILED

    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onClose) {
                Icon(Icons.Filled.KeyboardArrowDown, contentDescription = "Close player")
            }
            Text(
                playlist ?: "",
                style = MaterialTheme.typography.labelLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
                modifier = Modifier.weight(1f),
            )
            Box(Modifier.size(48.dp))
        }
        Box(
            Modifier
                .padding(vertical = 12.dp)
                .widthIn(max = 360.dp)
                .fillMaxWidth()
                .aspectRatio(1f)
                .clip(RoundedCornerShape(12.dp))
                .background(MaterialTheme.colorScheme.surfaceVariant),
            contentAlignment = Alignment.Center,
        ) {
            val image = cover
            if (image != null) {
                Image(image, contentDescription = "Cover", contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
            } else {
                Icon(PlayerIcons.Music, contentDescription = null, modifier = Modifier.size(96.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Text(
            now?.title ?: "Opening…",
            style = MaterialTheme.typography.titleLarge,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag("player-title"),
        )
        val below = when {
            failed -> queue.problem?.text ?: "This could not be played."
            message != null -> message
            else -> listOfNotNull(now?.artist, now?.album).joinToString(" · ").ifEmpty { null }
        }
        below?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                textAlign = TextAlign.Center,
                color = if (failed || message != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag("player-below"),
            )
        }
        Position(queue)
        Controls(queue)
    }
}

/** The position slider and the times; while dragging, the slider follows the finger only. */
@Composable
private fun Position(queue: QueueState) {
    val playback = queue.playback
    val duration = playback.durationMs ?: 0
    val position = if (queue.finished) duration else playback.positionMs
    var dragging by remember { mutableStateOf<Float?>(null) }
    // After a seek, its spot shows until the player gets there (or 3 s pass).
    var sought by remember { mutableStateOf<Pair<Long, Long>?>(null) }
    val pending = sought?.takeIf { (target, at) ->
        abs(position - target) > 1500 && SystemClock.uptimeMillis() - at < 3000
    }?.first
    val shown = dragging?.toLong() ?: pending ?: position
    if (duration > 0) {
        Slider(
            value = shown.coerceIn(0, duration).toFloat(),
            onValueChange = { dragging = it },
            onValueChangeFinished = {
                dragging?.let { target ->
                    sought = target.toLong() to SystemClock.uptimeMillis()
                    Playback.seek(target.toLong())
                }
                dragging = null
            },
            valueRange = 0f..duration.toFloat(),
            modifier = Modifier.fillMaxWidth().testTag("player-position").semantics { contentDescription = "Position" },
        )
    }
    Row(Modifier.fillMaxWidth().padding(top = if (duration > 0) 0.dp else 12.dp)) {
        Text(time(shown), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("player-elapsed"))
        Box(Modifier.weight(1f))
        if (duration > 0) {
            Text("-" + time(duration - shown.coerceIn(0, duration)), style = MaterialTheme.typography.bodySmall)
        }
    }
}

@Composable
private fun Controls(queue: QueueState) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 8.dp),
        horizontalArrangement = Arrangement.SpaceEvenly,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val on = MaterialTheme.colorScheme.primary
        val off = LocalContentColor.current
        IconButton(onClick = { Playback.setShuffle(!queue.shuffle) }, modifier = Modifier.testTag("shuffle")) {
            Icon(
                PlayerIcons.Shuffle,
                contentDescription = if (queue.shuffle) "Shuffle on" else "Shuffle off",
                tint = if (queue.shuffle) on else off,
            )
        }
        IconButton(onClick = { Playback.previous() }) {
            Icon(PlayerIcons.Previous, contentDescription = "Previous")
        }
        FilledIconButton(onClick = { if (queue.wantsPlay) Playback.pause() else Playback.resume() }, modifier = Modifier.size(64.dp)) {
            if (queue.wantsPlay) {
                Icon(PlayerIcons.Pause, contentDescription = "Pause", modifier = Modifier.size(36.dp))
            } else {
                Icon(Icons.Filled.PlayArrow, contentDescription = "Play", modifier = Modifier.size(36.dp))
            }
        }
        IconButton(onClick = { Playback.next() }, enabled = queue.hasNext) {
            Icon(PlayerIcons.Next, contentDescription = "Next")
        }
        val (icon, label) = when (queue.repeat) {
            RepeatMode.OFF -> PlayerIcons.Repeat to "Repeat off"
            RepeatMode.ALL -> PlayerIcons.Repeat to "Repeat all"
            RepeatMode.ONE -> PlayerIcons.RepeatOne to "Repeat one"
        }
        IconButton(
            onClick = {
                Playback.setRepeat(
                    when (queue.repeat) {
                        RepeatMode.OFF -> RepeatMode.ALL
                        RepeatMode.ALL -> RepeatMode.ONE
                        RepeatMode.ONE -> RepeatMode.OFF
                    },
                )
            },
            modifier = Modifier.testTag("repeat"),
        ) {
            Icon(icon, contentDescription = label, tint = if (queue.repeat == RepeatMode.OFF) off else on)
        }
    }
    Speed(queue.playback.speed)
}

@Composable
private fun Speed(speed: Double) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { open = true }, modifier = Modifier.testTag("speed")) {
            Text("Speed ${speedText(speed)}")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            SPEEDS.forEach { choice ->
                DropdownMenuItem(
                    text = { Text(speedText(choice), fontWeight = if (abs(choice - speed) < 0.01) FontWeight.Bold else null) },
                    onClick = {
                        open = false
                        Playback.setSpeed(choice)
                    },
                )
            }
        }
    }
}

/** One queue entry: tap to play it; the menu plays it next, moves or removes it. */
@Composable
private fun QueueRow(
    index: Int,
    row: ItemSummary?,
    known: Boolean,
    playing: Boolean,
    last: Boolean,
    onPlay: () -> Unit,
    onPlayNext: () -> Unit,
    onMove: (Int) -> Unit,
    onRemove: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onPlay).padding(start = 16.dp).testTag("queue-$index"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 10.dp)) {
            Text(
                when {
                    row != null -> row.title
                    known -> "Not in the library any more"
                    else -> ""
                },
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                fontWeight = if (playing) FontWeight.Bold else null,
                color = if (playing) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
            )
            row?.artist?.let {
                Text(it, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Box {
            IconButton(onClick = { menu = true }, modifier = Modifier.testTag("queue-menu-$index")) {
                Icon(Icons.Filled.MoreVert, contentDescription = "More")
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                if (!playing) {
                    DropdownMenuItem(text = { Text("Play next") }, onClick = { menu = false; onPlayNext() })
                }
                if (index > 0) {
                    DropdownMenuItem(text = { Text("Move up") }, onClick = { menu = false; onMove(index - 1) })
                }
                if (!last) {
                    DropdownMenuItem(text = { Text("Move down") }, onClick = { menu = false; onMove(index + 1) })
                }
                DropdownMenuItem(text = { Text("Remove") }, onClick = { menu = false; onRemove() })
            }
        }
    }
}

/** 1x, 0.75x, 1.5x. */
private fun speedText(speed: Double): String =
    (if (speed == speed.toLong().toDouble()) speed.toLong().toString() else speed.toString()) + "x"

/** 3:07, or 1:02:03 from an hour. */
private fun time(ms: Long): String {
    val seconds = ms.coerceAtLeast(0) / 1000
    val (hours, minutes, rest) = Triple(seconds / 3600, seconds / 60 % 60, seconds % 60)
    return if (hours > 0) "%d:%02d:%02d".format(hours, minutes, rest) else "%d:%02d".format(minutes, rest)
}
