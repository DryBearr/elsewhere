package io.github.drybearr.elsewhere

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.elsewhere.categories
import uniffi.elsewhere.coreStatus

/** The library's tabs, and what the core has loaded. */
@Composable
fun LibraryScreen() {
    val names by produceState(emptyList<String>()) {
        value = withContext(Dispatchers.IO) { runCatching { categories().map { it.name } }.getOrDefault(emptyList()) }
    }
    val status by produceState("") {
        value = withContext(Dispatchers.IO) {
            Core.startError?.let { "The core did not start: $it" } ?: coreStatus().let {
                "yt-dlp " + if (it.ytdlpStarted) "running" else "starts with the first download"
            }
        }
    }
    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("Library", style = MaterialTheme.typography.titleMedium)
        names.forEach { Text(it) }
        Text(status, style = MaterialTheme.typography.bodySmall)
    }
}
