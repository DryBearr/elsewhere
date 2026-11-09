package io.github.drybearr.elsewhere

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import io.github.drybearr.elsewhere.playback.Playback
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.elsewhere.ApiException
import uniffi.elsewhere.CorePlayer
import uniffi.elsewhere.DemoOptions
import uniffi.elsewhere.PlayerListener
import uniffi.elsewhere.PlayerState
import uniffi.elsewhere.PlayerStatus
import uniffi.elsewhere.QueueState
import uniffi.elsewhere.RepeatMode
import uniffi.elsewhere.JobState
import uniffi.elsewhere.JobStatus
import uniffi.elsewhere.addFile
import uniffi.elsewhere.categories
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
 * (breaks the core on purpose: the app must restart itself, see Health), player (plays
 * the debug build's test files through the real engine and sound output), or background
 * (plays two test songs again and again through Playback; SelfTestReceiver then plays a
 * call, a navigation voice, another app and unplugged headphones at it).
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
                    // 20 items of 4 s: still running when killed, even when the log arrives late.
                    "demo" -> startDemoJob(DemoOptions(20u, 4000u, 3u, 2u))
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
                    "background" -> background(context)
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

    /** One line per playlist player state. */
    fun onQueue(state: QueueState) {
        if (!enabled) return
        val playback = state.playback
        Log.i(TAG, "QUEUE rev=${state.revision} entries=${state.entries.size} current=${state.current} item=${state.nowPlaying?.itemId} status=${playback.status} wants=${state.wantsPlay} pos=${playback.positionMs} dur=${playback.durationMs} volume=${playback.volume} finished=${state.finished} next=${state.hasNext} keeps=${Playback.keepsGoing} problem=${state.problem?.kind} title=${state.nowPlaying?.title}")
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

    /** Adds two test songs to the Music tab and plays them again and again through Playback. */
    private fun background(context: Context) {
        val music = categories().first { it.name == "Music" }
        val folder = File(Core.media, "${music.name}/selftest-background").apply { mkdirs() }
        val items = listOf("long.m4a", "song.m4a").map { name ->
            val file = File(folder, name)
            context.assets.open(name).use { input -> file.outputStream().use { input.copyTo(it) } }
            addFile(music.id, file.path)
        }
        main.post {
            Playback.playItems(items, 0) { Playback.setRepeat(RepeatMode.ALL) }
        }
    }

    private val main = Handler(Looper.getMainLooper())

    /** Another "app" taking the audio focus from Playback (a call, a navigation voice, a player). */
    private var thief: AudioFocusRequest? = null

    /** What SelfTestReceiver was sent (main thread). */
    fun act(context: Context, action: String?) {
        if (!enabled) return
        val audio = context.getSystemService(AudioManager::class.java)
        when (action) {
            "focus-transient" -> steal(audio, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT, AudioAttributes.USAGE_VOICE_COMMUNICATION)
            "focus-duck" -> steal(audio, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK, AudioAttributes.USAGE_ASSISTANCE_NAVIGATION_GUIDANCE)
            "focus-permanent" -> steal(audio, AudioManager.AUDIOFOCUS_GAIN, AudioAttributes.USAGE_MEDIA)
            "focus-return" -> {
                thief?.let { audio.abandonAudioFocusRequest(it) }
                thief = null
            }
            "noisy" -> Playback.becameNoisy()
            "repeat-off" -> Playback.setRepeat(RepeatMode.OFF)
            "report" -> Playback.state.value?.let { onQueue(it) }
            else -> {
                Log.i(TAG, "ERROR unknown self-test action $action")
                return
            }
        }
        Log.i(TAG, "ACTED $action")
    }

    private fun steal(audio: AudioManager, gain: Int, usage: Int) {
        thief?.let { audio.abandonAudioFocusRequest(it) }
        val request = AudioFocusRequest.Builder(gain)
            .setAudioAttributes(AudioAttributes.Builder().setUsage(usage).build())
            .setOnAudioFocusChangeListener({}, main)
            .build()
        thief = request
        val granted = audio.requestAudioFocus(request)
        Log.i(TAG, "STOLE gain=$gain granted=$granted")
    }
}

/**
 * Debug builds only (src/debug/AndroidManifest.xml):
 * `adb shell am broadcast -n io.github.drybearr.elsewhere/.SelfTestReceiver --es action <action>`
 * with focus-transient, focus-duck, focus-permanent, focus-return, noisy, repeat-off or report.
 */
class SelfTestReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        SelfTest.act(context, intent.getStringExtra("action"))
    }
}
