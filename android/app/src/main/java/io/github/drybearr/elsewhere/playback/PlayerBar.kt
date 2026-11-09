package io.github.drybearr.elsewhere.playback

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.elsewhere.PlayerStatus

/**
 * The small player above the tabs while something is queued: what plays, its progress,
 * previous, play/pause, next and stop. Plugged in by one line in MainActivity.
 */
@Composable
fun PlayerBar() {
    val state by Playback.state.collectAsState()
    val message by Playback.message.collectAsState()
    val queue = state
    val index = queue?.current?.toInt()
    if (queue == null || index == null || index >= queue.entries.size) {
        // Nothing queued: only a refusal shows (for example no sound during a call).
        message?.let { text ->
            Surface(tonalElevation = 3.dp, modifier = Modifier.fillMaxWidth()) {
                Text(text, Modifier.padding(12.dp), style = MaterialTheme.typography.bodySmall)
            }
        }
        return
    }
    val playback = queue.playback
    val now = queue.nowPlaying
    val failed = queue.finished && playback.status == PlayerStatus.FAILED
    val below = when {
        failed -> queue.problem?.text ?: "This could not be played."
        message != null -> message
        queue.finished -> "Finished"
        else -> now?.artist
    }
    Surface(tonalElevation = 3.dp, modifier = Modifier.fillMaxWidth()) {
        Column {
            val duration = playback.durationMs ?: 0
            if (duration > 0) {
                val done = (playback.positionMs.toFloat() / duration).coerceIn(0f, 1f)
                LinearProgressIndicator(
                    progress = { if (queue.finished) 1f else done },
                    modifier = Modifier.fillMaxWidth().semantics { contentDescription = "Progress" },
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f).padding(start = 16.dp, top = 6.dp, bottom = 6.dp)) {
                    Text(
                        now?.title ?: "Opening…",
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier.testTag("now-title"),
                    )
                    below?.let {
                        Text(
                            it,
                            modifier = Modifier.testTag("now-below"),
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall,
                            color = if (failed || message != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                IconButton(onClick = { Playback.previous() }) {
                    Icon(PlayerIcons.Previous, contentDescription = "Previous")
                }
                if (queue.wantsPlay) {
                    IconButton(onClick = { Playback.pause() }) {
                        Icon(PlayerIcons.Pause, contentDescription = "Pause")
                    }
                } else {
                    IconButton(onClick = { Playback.resume() }) {
                        Icon(Icons.Filled.PlayArrow, contentDescription = "Play")
                    }
                }
                IconButton(onClick = { Playback.next() }, enabled = queue.hasNext) {
                    Icon(PlayerIcons.Next, contentDescription = "Next")
                }
                IconButton(onClick = { Playback.stop() }) {
                    Icon(Icons.Filled.Close, contentDescription = "Stop")
                }
            }
        }
    }
}
