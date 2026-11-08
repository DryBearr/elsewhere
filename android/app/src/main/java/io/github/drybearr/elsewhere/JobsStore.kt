package io.github.drybearr.elsewhere

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.elsewhere.JobListener
import uniffi.elsewhere.JobState
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.ProblemKind
import uniffi.elsewhere.cancelJob
import uniffi.elsewhere.discardJob
import uniffi.elsewhere.forgetJob
import uniffi.elsewhere.resumeJob

/**
 * The one place the app keeps job states. The core sends a job's whole state on every
 * change; this keeps the newest revision of each job and every screen reads it from here.
 * No screen keeps its own copy.
 */
object JobsStore : JobListener {
    private val shown = MutableStateFlow<Map<Long, JobState>>(emptyMap())
    val jobs: StateFlow<Map<Long, JobState>> = shown.asStateFlow()

    /** Called on the core's delivery thread; StateFlow.update is safe from any thread. */
    override fun onJob(job: JobState) {
        shown.update { jobs ->
            val old = jobs[job.id]
            if (old != null && old.revision >= job.revision) jobs else jobs + (job.id to job)
        }
        SelfTest.onJob(job)
        // A bug may have left a part broken: see whether the app must restart.
        if (job.status == JobStatus.FAILED && job.lastError?.kind == ProblemKind.BUG) Health.check()
    }

    fun cancel(id: Long) = act("cancel") { cancelJob(id) }

    fun resume(id: Long) = act("resume") { resumeJob(id) }

    fun discard(id: Long) = act("discard") { discardJob(id) }

    /** Removes a finished job. The core sends nothing for a removed job, so drop it here. */
    fun forget(id: Long) = act("forget") {
        if (forgetJob(id)) shown.update { it - id }
    }

    private fun act(name: String, call: () -> Unit) {
        Core.io.launch {
            try {
                call()
            } catch (e: Exception) {
                Log.w("Elsewhere", "$name failed: ${describe(e)}")
            }
        }
    }
}

val JobStatus.isFinished: Boolean
    get() = this == JobStatus.DONE || this == JobStatus.FAILED || this == JobStatus.CANCELLED
