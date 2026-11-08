package io.github.drybearr.elsewhere

import android.util.Log
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.elsewhere.DemoOptions
import uniffi.elsewhere.JobState
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.discardJob
import uniffi.elsewhere.job
import uniffi.elsewhere.jobs
import uniffi.elsewhere.resumeJob
import uniffi.elsewhere.simulateBrokenCore
import uniffi.elsewhere.startDemoJob
import uniffi.elsewhere.ytdlpVersion

/**
 * The CI check of the job path through the real app (scripts/selftest_android.sh). Debug
 * builds only: every job state the store receives is logged as one line (tag
 * ElsewhereJobs), and the intent extra "selftest" makes the app act at launch:
 * demo, resume (every Interrupted job), discard (every Interrupted job), ytdlp, or broken
 * (breaks the core on purpose: the app must restart itself, see Health).
 */
object SelfTest {
    private const val TAG = "ElsewhereJobs"

    @Volatile
    var enabled = false

    fun run(mode: String?, restarted: Boolean) {
        if (!enabled) return
        if (restarted) Log.i(TAG, "RESTARTED pid=${android.os.Process.myPid()}")
        if (mode == null) return
        Core.io.launch {
            try {
                when (mode) {
                    "demo" -> startDemoJob(DemoOptions(20u, 500u, 3u, 2u))
                    "resume" -> interrupted().forEach { resumeJob(it.id) }
                    "discard" -> interrupted().forEach { discardJob(it.id) }
                    "ytdlp" -> Log.i(TAG, "YTDLP ${ytdlpVersion()}")
                    "broken" -> {
                        // Restarting brings the app back only while it is on screen.
                        while (!Health.isVisible) delay(50)
                        Log.i(TAG, "BROKEN pid=${android.os.Process.myPid()}")
                        simulateBrokenCore()
                        Health.checkNow(force = true)
                    }
                }
            } catch (e: Exception) {
                Log.i(TAG, "ERROR $mode: ${describe(e)}")
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
