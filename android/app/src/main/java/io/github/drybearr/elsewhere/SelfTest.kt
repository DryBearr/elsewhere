package io.github.drybearr.elsewhere

import android.util.Log
import kotlinx.coroutines.launch
import uniffi.elsewhere.DemoOptions
import uniffi.elsewhere.JobState
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.discardJob
import uniffi.elsewhere.job
import uniffi.elsewhere.jobs
import uniffi.elsewhere.resumeJob
import uniffi.elsewhere.startDemoJob
import uniffi.elsewhere.ytdlpVersion

/**
 * The CI check of the job path through the real app (scripts/selftest_android.sh). Debug
 * builds only: every job state the store receives is logged as one line (tag
 * ElsewhereJobs), and the intent extra "selftest" makes the app act at launch:
 * demo, resume (every Interrupted job), discard (every Interrupted job) or ytdlp.
 */
object SelfTest {
    private const val TAG = "ElsewhereJobs"

    @Volatile
    var enabled = false

    fun run(mode: String?) {
        if (!enabled || mode == null) return
        Core.io.launch {
            try {
                when (mode) {
                    "demo" -> startDemoJob(DemoOptions(20u, 500u, 3u, 2u))
                    "resume" -> interrupted().forEach { resumeJob(it.id) }
                    "discard" -> interrupted().forEach { discardJob(it.id) }
                    "ytdlp" -> Log.i(TAG, "YTDLP ${ytdlpVersion()}")
                }
            } catch (e: Exception) {
                Log.i(TAG, "ERROR $mode: ${e.message}")
            }
        }
    }

    /** One line per state; for a final state also whether the store equals the core. */
    fun onJob(state: JobState) {
        if (!enabled) return
        Log.i(TAG, "STATE id=${state.id} rev=${state.revision} status=${state.status} done=${state.done} failed=${state.failed} total=${state.total}")
        if (state.status.isFinished) {
            Core.io.launch {
                val shown = JobsStore.jobs.value[state.id]
                val core = runCatching { job(state.id) }.getOrNull()
                Log.i(TAG, "FINAL id=${state.id} status=${state.status} done=${state.done} failed=${state.failed} log=${state.logLines} match=${shown == core}")
            }
        }
    }

    private fun interrupted() = jobs().filter { it.status == JobStatus.INTERRUPTED }
}
