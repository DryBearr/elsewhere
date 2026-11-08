package io.github.drybearr.elsewhere

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Card
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.elsewhere.JobLogLine
import uniffi.elsewhere.JobState
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.LogLevel
import uniffi.elsewhere.jobLogs

/** Every job, newest first. Tapping one opens its log. */
@Composable
fun DownloadsScreen(onOpen: (Long) -> Unit) {
    val jobs by JobsStore.jobs.collectAsState()
    val newestFirst = jobs.values.sortedByDescending { it.id }
    if (newestFirst.isEmpty()) {
        Text("No downloads yet.", Modifier.padding(16.dp))
        return
    }
    LazyColumn(
        Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
    ) {
        items(newestFirst, key = { it.id }) { job -> JobCard(job, Modifier.clickable { onOpen(job.id) }) }
    }
}

/** One job: title, status, progress, the current item, the last error and its buttons. */
@Composable
fun JobCard(job: JobState, modifier: Modifier = Modifier) {
    Card(modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(job.title, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleSmall)
                Text(label(job.status), style = MaterialTheme.typography.labelMedium)
            }
            val total = job.total
            val finishedItems = job.done + job.failed
            if (total != null && total > 0) {
                LinearProgressIndicator(progress = { finishedItems.toFloat() / total }, Modifier.fillMaxWidth())
            }
            val counts = buildString {
                append("$finishedItems of ${total ?: "?"}")
                if (job.failed > 0) append(" · ${job.failed} failed")
            }
            Text(counts, style = MaterialTheme.typography.bodySmall)
            job.current?.let { item ->
                Text("${item.position}. ${item.title}", maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
                item.totalBytes?.takeIf { it > 0 }?.let { size ->
                    LinearProgressIndicator(progress = { item.bytes.toFloat() / size }, Modifier.fillMaxWidth())
                }
                Text(transfer(item.bytes, item.totalBytes, item.speed, item.eta), style = MaterialTheme.typography.bodySmall)
            }
            job.lastError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            Row {
                when (job.status) {
                    JobStatus.QUEUED, JobStatus.RUNNING -> TextButton({ JobsStore.cancel(job.id) }) { Text("Cancel") }
                    JobStatus.INTERRUPTED -> {
                        TextButton({ JobsStore.resume(job.id) }) { Text("Resume") }
                        TextButton({ JobsStore.discard(job.id) }) { Text("Discard") }
                    }
                    JobStatus.FAILED -> {
                        TextButton({ JobsStore.resume(job.id) }) { Text("Retry") }
                        TextButton({ JobsStore.forget(job.id) }) { Text("Remove") }
                    }
                    JobStatus.DONE, JobStatus.CANCELLED -> TextButton({ JobsStore.forget(job.id) }) { Text("Remove") }
                }
            }
        }
    }
}

/** One job with its whole log (info, warnings, errors), which grows while it runs. */
@Composable
fun JobDetailScreen(id: Long, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val jobs by JobsStore.jobs.collectAsState()
    val job = jobs[id]
    var lines by remember(id) { mutableStateOf(emptyList<JobLogLine>()) }
    LaunchedEffect(id, job?.logLines) {
        val since = lines.lastOrNull()?.number ?: 0L
        val new = withContext(Dispatchers.IO) { runCatching { jobLogs(id, since) }.getOrDefault(emptyList()) }
        if (new.isNotEmpty()) lines = lines + new
    }
    Column(Modifier.fillMaxSize().padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        TextButton(onBack) { Text("‹ Downloads") }
        if (job == null) {
            Text("This job was removed.")
            return
        }
        JobCard(job)
        Text("Log", style = MaterialTheme.typography.titleSmall)
        LazyColumn(Modifier.fillMaxSize()) {
            items(lines, key = { it.number }) { line ->
                SelectionContainer {
                    Text("${line.number}. ${line.message}", color = color(line.level), style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

private fun label(status: JobStatus) = when (status) {
    JobStatus.QUEUED -> "Waiting"
    JobStatus.RUNNING -> "Running"
    JobStatus.INTERRUPTED -> "Interrupted"
    JobStatus.DONE -> "Done"
    JobStatus.FAILED -> "Failed"
    JobStatus.CANCELLED -> "Cancelled"
}

@Composable
private fun color(level: LogLevel) = when (level) {
    LogLevel.ERROR -> MaterialTheme.colorScheme.error
    LogLevel.WARNING -> Color(0xFFB26A00)
    else -> MaterialTheme.colorScheme.onSurface
}

private fun transfer(bytes: Long, total: Long?, speed: Double?, eta: Long?): String = buildString {
    append(size(bytes.toDouble()))
    total?.let { append(" of ${size(it.toDouble())}") }
    speed?.let { append(" · ${size(it)}/s") }
    eta?.let { append(" · ${it / 60}:${"%02d".format(it % 60)} left") }
}

private fun size(bytes: Double): String = when {
    bytes >= 1e9 -> "%.1f GB".format(bytes / 1e9)
    bytes >= 1e6 -> "%.1f MB".format(bytes / 1e6)
    bytes >= 1e3 -> "%.0f kB".format(bytes / 1e3)
    else -> "%.0f B".format(bytes)
}
