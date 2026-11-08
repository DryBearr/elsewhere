package io.github.drybearr.elsewhere

import android.content.Context
import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import java.io.File
import java.io.InputStream
import kotlin.concurrent.thread
import org.json.JSONObject
import uniffi.elsewhere.ByteSource
import uniffi.elsewhere.CancelToken
import uniffi.elsewhere.EventListener
import uniffi.elsewhere.Folders
import uniffi.elsewhere.YtdlpEvent
import uniffi.elsewhere.startYtdlp
import uniffi.elsewhere.unpackPython
import uniffi.elsewhere.ytdlpInfo

/** yt-dlp in the Rust core: started once, then used by YtdlpScreen. */
object Ytdlp {
    var status by mutableStateOf("Starting yt-dlp…")
        private set
    var ready by mutableStateOf(false)
        private set
    private var started = false

    /** Unpacks Python on the first start after an install or update, then starts yt-dlp. */
    @Synchronized
    fun start(context: Context) {
        if (started) return
        started = true
        thread(name = "ytdlp-start") {
            val text = try {
                val home = context.assets.open("python.tar").use { asset ->
                    unpackPython(AssetSource(asset), context.filesDir.path)
                }
                val temp = File(context.cacheDir, "tmp").apply { mkdirs() }
                val version = startYtdlp(
                    Folders(
                        data = context.filesDir.path,
                        cache = context.cacheDir.path,
                        temp = temp.path,
                        pythonHome = home,
                        programs = context.applicationInfo.nativeLibraryDir,
                    ),
                )
                ready = true
                "yt-dlp $version is ready."
            } catch (e: Exception) {
                "yt-dlp could not start: ${e.message}"
            }
            status = text
            // The emulator check in .github/workflows/mobile.yml looks for this line.
            Log.i("Elsewhere", text)
        }
    }

    /** Title, uploader and length of a link, or the error. Blocks; call it off the main thread. */
    fun lookUp(link: String): String = try {
        val json = JSONObject(ytdlpInfo(link, listOf("--playlist-items", "1"), NoEvents, CancelToken()))
        listOf("title", "uploader", "duration_string")
            .map { json.optString(it) }
            .filter { it.isNotEmpty() }
            .joinToString("\n")
    } catch (e: Exception) {
        e.message ?: e.toString()
    }
}

/** Hands the python.tar asset to the Rust core, piece by piece. */
private class AssetSource(private val input: InputStream) : ByteSource {
    override fun read(max: UInt): ByteArray {
        val buffer = ByteArray(max.toInt())
        val count = input.read(buffer)
        return if (count <= 0) ByteArray(0) else buffer.copyOf(count)
    }
}

private object NoEvents : EventListener {
    override fun onEvent(event: YtdlpEvent) {}
}

/** Paste a link, get its title: shows that yt-dlp works on this phone. */
@Composable
fun YtdlpScreen() {
    val context = LocalContext.current
    LaunchedEffect(Unit) { Ytdlp.start(context.applicationContext) }
    var link by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf("") }

    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("yt-dlp", style = MaterialTheme.typography.titleMedium)
        Text(Ytdlp.status)
        OutlinedTextField(
            value = link,
            onValueChange = { link = it },
            label = { Text("Video link") },
            singleLine = true,
            enabled = Ytdlp.ready,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
        Button(
            enabled = Ytdlp.ready && !busy && link.isNotBlank(),
            onClick = {
                busy = true
                result = "Looking it up…"
                val url = link.trim()
                thread(name = "ytdlp-info") {
                    result = Ytdlp.lookUp(url)
                    busy = false
                }
            },
        ) { Text("Get info") }
        if (result.isNotEmpty()) {
            SelectionContainer { Text(result) }
        }
    }
}
