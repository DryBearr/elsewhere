package io.github.drybearr.elsewhere

import android.content.Context
import android.util.Log
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.elsewhere.ApiException
import uniffi.elsewhere.CorePlayer
import uniffi.elsewhere.DemoOptions
import uniffi.elsewhere.PlayerListener
import uniffi.elsewhere.PlayerState
import uniffi.elsewhere.PlayerStatus
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
 * demo, long-demo, resume (every Interrupted job), discard (every Interrupted job), ytdlp, broken
 * (breaks the core on purpose: the app must restart itself, see Health), or player (plays
 * the debug build's test files through the real engine and sound output).
 */
object SelfTest {
    private const val TAG = "ElsewhereJobs"

    @Volatile
    var enabled = false

    /** Players of the self-test, kept so they live until the process ends. */
    private val players = mutableListOf<CorePlayer>()

    fun run(context: Context, mode: String?, restarted: Boolean) {
        if (!enabled) return
        if (restarted) Log.i(TAG, "RESTARTED pid=${android.os.Process.myPid()}")
        if (mode == null) return
        Core.io.launch {
            try {
                when (mode) {
                    // 20 items of 1.5 s: long enough to be killed halfway on a slow emulator.
                    "demo" -> startDemoJob(DemoOptions(20u, 1500u, 3u, 2u))
                    // Runs for minutes: it is still running when the app is killed.
                    "long-demo" -> startDemoJob(DemoOptions(200u, 1000u, null, null))
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
                    "player" -> player(context)
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

    /**
     * Copies two test files into the media folder and plays each to its end, logging every
     * state; then checks that a file outside the media folder is refused.
     */
    private suspend fun player(context: Context) {
        val folder = File(Core.media, "selftest").apply { mkdirs() }
        val player = CorePlayer()
        players += player
        player.setListener(object : PlayerListener {
            override fun onState(state: PlayerState) {
                Log.i(TAG, "PLAYER file=${state.file?.substringAfterLast('/')} rev=${state.revision} status=${state.status} pos=${state.positionMs} dur=${state.durationMs} video=${state.hasVideo} audio=${state.hasAudio} problem=${state.problem?.detail}")
            }
        })
        for (name in listOf("song.m4a", "clip.webm")) {
            val file = File(folder, name)
            context.assets.open(name).use { input -> file.outputStream().use { input.copyTo(it) } }
            val started = System.currentTimeMillis()
            player.open(file.path, 0, true)
            val deadline = started + 30_000
            while (player.state().status !in setOf(PlayerStatus.ENDED, PlayerStatus.FAILED) &&
                System.currentTimeMillis() < deadline
            ) {
                delay(20)
            }
            val state = player.state()
            val took = System.currentTimeMillis() - started
            Log.i(TAG, "PLAYED file=$name status=${state.status} pos=${state.positionMs} dur=${state.durationMs} took=$took problem=${state.problem?.detail}")
        }
        val refused = try {
            player.open("/system/etc/hosts", 0, true)
            "no"
        } catch (e: ApiException.Failed) {
            e.problem.kind.toString()
        }
        Log.i(TAG, "PLAYER refused=$refused")
    }
}
