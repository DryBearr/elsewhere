package io.github.drybearr.elsewhere

import android.content.Context
import android.content.pm.ApplicationInfo
import android.util.Log
import java.io.File
import java.io.IOException
import java.io.InputStream
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import uniffi.elsewhere.ApiException
import uniffi.elsewhere.Folders
import uniffi.elsewhere.PythonBundle
import uniffi.elsewhere.setJobListener
import uniffi.elsewhere.start as startCore

/** The Rust core. Calls into it block, so they go through [io], never the main thread. */
object Core {
    /** Where screens run their calls into the core. */
    val io = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Where saved media goes; players open files inside it only. */
    lateinit var media: File
        private set

    /** Why the core did not start, if it did not. */
    var startError: String? = null
        private set

    /**
     * Starts the core: folders, database, jobs. Cheap, so it runs on the main thread at app
     * start; it must run before anything else because it sets environment variables.
     * Python and yt-dlp start later, on first use, from the APK's python.tar.
     */
    fun start(context: Context) {
        SelfTest.enabled = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
        media = File(context.getExternalFilesDir(null) ?: context.filesDir, "media")
        val folders = Folders(
            data = context.filesDir.path,
            cache = context.cacheDir.path,
            temp = File(context.cacheDir, "tmp").path,
            media = media.path,
            pythonHome = null,
            pythonPaths = emptyList(),
            programs = context.applicationInfo.nativeLibraryDir,
        )
        try {
            startCore(folders, AssetBundle(context.applicationContext))
            setJobListener(JobsStore)
        } catch (e: Exception) {
            startError = describe(e)
            Log.e("Elsewhere", "the core did not start", e)
        }
    }
}

/** What to show for a failed call: the core's problem text, or the exception's message. */
fun describe(error: Throwable): String =
    (error as? ApiException.Failed)?.problem?.text ?: error.message ?: error.toString()

/** Hands the APK's python.tar to the core, piece by piece, when it first needs Python. */
private class AssetBundle(private val context: Context) : PythonBundle {
    private var input: InputStream? = null

    @Synchronized
    override fun rewind(): Boolean = try {
        input?.close()
        input = context.assets.open("python.tar")
        true
    } catch (e: IOException) {
        false
    }

    @Synchronized
    override fun read(max: UInt): ByteArray {
        val buffer = ByteArray(max.toInt())
        val count = input?.read(buffer) ?: -1
        if (count <= 0) {
            input?.close()
            input = null
            return ByteArray(0)
        }
        return buffer.copyOf(count)
    }
}
