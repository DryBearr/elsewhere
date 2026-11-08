package io.github.drybearr.elsewhere

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.elsewhere.DemoOptions
import uniffi.elsewhere.MediaFormat
import uniffi.elsewhere.PlaylistOptions
import uniffi.elsewhere.savePlaylist
import uniffi.elsewhere.startDemoJob

private val qualities = listOf<Int?>(null, 1080, 720, 480, 360)

/** Save a playlist (or one video): link, audio or video, quality, lyrics. */
@Composable
fun AddScreen(onStarted: () -> Unit) {
    var link by rememberSaveable { mutableStateOf("") }
    var audio by rememberSaveable { mutableStateOf(true) }
    var quality by rememberSaveable { mutableStateOf<Int?>(720) }
    var lyrics by rememberSaveable { mutableStateOf(true) }
    var error by rememberSaveable { mutableStateOf<String?>(null) }

    fun run(call: () -> Unit) {
        error = null
        Core.io.launch {
            val result = runCatching(call)
            withContext(Dispatchers.Main) {
                result.onSuccess { onStarted() }.onFailure { error = it.message ?: it.toString() }
            }
        }
    }

    Column(
        Modifier.padding(16.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Save a playlist or video", style = MaterialTheme.typography.titleMedium)
        OutlinedTextField(
            value = link,
            onValueChange = { link = it },
            label = { Text("YouTube or YouTube Music link") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(selected = audio, onClick = { audio = true }, label = { Text("Audio (m4a)") })
            FilterChip(selected = !audio, onClick = { audio = false }, label = { Text("Video (mp4)") })
        }
        if (!audio) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                qualities.forEach { height ->
                    FilterChip(selected = quality == height, onClick = { quality = height }, label = { Text(height?.let { "${it}p" } ?: "Best") })
                }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Switch(checked = lyrics, onCheckedChange = { lyrics = it })
            Text("Find lyrics")
        }
        Button(onClick = {
            val format = if (audio) MediaFormat.Audio else MediaFormat.Video(quality?.toUInt())
            run { savePlaylist(link, PlaylistOptions(format, lyrics, null)) }
        }, enabled = link.isNotBlank()) { Text("Save") }
        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }

        HorizontalDivider()
        Text("Check downloads without network:", style = MaterialTheme.typography.bodySmall)
        OutlinedButton(onClick = { run { startDemoJob(DemoOptions(10u, 400u, 3u, 2u)) } }) { Text("Run a demo job") }
    }
}
