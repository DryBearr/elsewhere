package io.github.drybearr.elsewhere.playback

import android.app.Application
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Surface
import io.github.drybearr.elsewhere.SelfTest
import io.github.drybearr.elsewhere.describe
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.Executors
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import uniffi.elsewhere.CoreQueue
import uniffi.elsewhere.QueueEntries
import uniffi.elsewhere.QueueListener
import uniffi.elsewhere.QueueState
import uniffi.elsewhere.RepeatMode

/**
 * The app's playlist player: one CoreQueue for the whole app. It plays on in the background
 * through PlaybackService (notification, lock screen, Bluetooth and headphone buttons).
 * Call it on the main thread; the calls into the core run one after the other on its own
 * thread (in the order they were made), and states come back on the main thread.
 *
 * It also does what Android expects of a media app:
 * - asks for audio focus before playing: a call pauses it and it plays on after the call,
 *   another app playing pauses it for good, a navigation voice makes it quieter;
 * - pauses when the headphones are unplugged (or Bluetooth drops).
 *
 * Plugged in by one line in ElsewhereApplication (Playback.init) and the service in the
 * manifest.
 */
object Playback {
    private const val TAG = "Elsewhere"

    /** The volume while another app speaks over us (a navigation voice). */
    private const val DUCKED = 20.0

    private lateinit var app: Application
    private val main = Handler(Looper.getMainLooper())

    private var queue: CoreQueue? = null

    /** The engine's number for VideoSurface, once the queue is made (0 before). */
    @Volatile
    private var target = 0L

    /** A Surface waiting for the queue to be made (main thread). */
    private var waitingSurface: Surface? = null
    /** The Surface mpv draws into now (the newest VideoView's). */
    private var shownSurface: Surface? = null

    /** One thread, so commands reach the queue in the order they were made. */
    private val commands = Executors.newSingleThreadExecutor { Thread(it, "elsewhere-playback") }
    private val shown = MutableStateFlow<QueueState?>(null)

    /** The newest state; null until something is played. */
    val state: StateFlow<QueueState?> = shown.asStateFlow()
    private val watchers = CopyOnWriteArrayList<() -> Unit>()

    private val told = MutableStateFlow<String?>(null)

    /** Why the last command did nothing (refused, no sound during a call); null after one works. */
    val message: StateFlow<String?> = told.asStateFlow()

    private val playing = MutableStateFlow<Long?>(null)

    /** The saved playlist the queue was made from, if it was. */
    val source: StateFlow<Long?> = playing.asStateFlow()

    private val listed = MutableStateFlow<QueueEntries?>(null)

    /** The queue's entries, while a screen wants them ([wantEntries]); else possibly old. */
    val entries: StateFlow<QueueEntries?> = listed.asStateFlow()
    private var entryScreens = 0
    private var fetching: Long? = null

    /** Commands that start playback and have not answered yet. */
    private var starting = 0

    fun init(application: Application) {
        app = application
    }

    /**
     * Whether playback goes on, is about to, or waits for a call to end: what keeps
     * PlaybackService in the foreground, so Android lets it play on.
     */
    val keepsGoing: Boolean
        get() = shown.value?.wantsPlay == true || starting > 0 || resumeAfterLoss

    /** Called on the main thread after every change of [state] or [keepsGoing]. */
    fun watch(watcher: () -> Unit) {
        watchers += watcher
    }

    fun unwatch(watcher: () -> Unit) {
        watchers -= watcher
    }

    // --- What screens, the service and the self-test call ------------------------------------

    fun playItems(items: List<Long>, start: Int, done: () -> Unit = {}) =
        play("play", done, { playing.value = null }) { it.play(items, start.toUInt(), true) }

    fun playCollection(collectionId: Long, startEntry: Long?, done: () -> Unit = {}) =
        play("play a playlist", done, { playing.value = collectionId }) {
            it.playCollection(collectionId, startEntry, true)
        }

    fun resume(done: () -> Unit = {}) = play("resume", done) { it.resume() }

    /** A pause the person asked for (also by a system button): no playing on after a call. */
    fun pause(done: () -> Unit = {}) {
        if (resumeAfterLoss) {
            resumeAfterLoss = false
            changed()
        }
        command("pause", done) { it.pause() }
    }

    fun next(done: () -> Unit = {}) = command("next", done) { it.next() }

    /** Plays the entry with this key (from a queue screen). */
    fun jump(key: Long, done: () -> Unit = {}) = play("jump", done) {
        it.jump(key)
        it.resume()
    }

    fun remove(key: Long, done: () -> Unit = {}) = command("remove", done) { it.remove(key) }

    fun moveEntry(key: Long, to: Int, done: () -> Unit = {}) =
        command("move", done) { it.moveEntry(key, to.coerceAtLeast(0).toUInt()) }

    fun playNext(key: Long, done: () -> Unit = {}) = command("play next", done) { it.playNext(key) }

    /** Adds an item (a song from a playlist) at the end; to an empty or ended queue, it plays. */
    fun add(itemId: Long, done: () -> Unit = {}) = play("add", done) { queue ->
        val idle = queue.state().let { it.entryCount == 0u || it.finished }
        queue.add(listOf(itemId))
        if (idle) queue.resume()
    }

    /** Adds an item right after what plays; to an empty or ended queue, it plays. */
    fun addNext(itemId: Long, done: () -> Unit = {}) = play("play next", done) { queue ->
        val idle = queue.state().let { it.entryCount == 0u || it.finished }
        queue.add(listOf(itemId))
        // A new entry goes at the end of the list (also when shuffled).
        if (!idle) queue.entries().entries.lastOrNull()?.let { entry -> queue.playNext(entry.key) }
        if (idle) queue.resume()
    }

    /**
     * A screen showing the queue's entries calls this with true while it shows (and false
     * after): only then are thousands of entries read again on every change.
     */
    fun wantEntries(on: Boolean) {
        entryScreens = (entryScreens + if (on) 1 else -1).coerceAtLeast(0)
        fetchEntries()
    }

    private fun fetchEntries() {
        val revision = shown.value?.entriesRevision ?: return
        if (entryScreens == 0 || listed.value?.revision == revision || fetching == revision) return
        fetching = revision
        commands.execute {
            val got = try {
                queue().entries()
            } catch (e: Exception) {
                Log.w(TAG, "entries failed: ${describe(e)}")
                null
            }
            main.post {
                fetching = null
                if (got != null && got.revision >= (listed.value?.revision ?: 0)) listed.value = got
                // Changed again meanwhile: once more.
                fetchEntries()
            }
        }
    }

    /** Starts the entry over; within its first 3 s, goes to the one before. */
    fun previous(done: () -> Unit = {}) = command("previous", done) { it.previous() }

    fun seek(positionMs: Long, done: () -> Unit = {}) = command("seek", done) { it.seek(positionMs) }

    fun setRepeat(repeat: RepeatMode, done: () -> Unit = {}) = command("repeat", done) { it.setRepeat(repeat) }

    fun setShuffle(shuffle: Boolean, done: () -> Unit = {}) = command("shuffle", done) { it.setShuffle(shuffle) }

    fun setSpeed(speed: Double, done: () -> Unit = {}) = command("speed", done) { it.setSpeed(speed) }

    /** Ends playback and empties the queue (the notification goes away). */
    fun stop(done: () -> Unit = {}) {
        if (resumeAfterLoss) {
            resumeAfterLoss = false
            changed()
        }
        command("stop", { playing.value = null; done() }) { it.stop() }
    }

    /** Headphones unplugged or Bluetooth gone: pause before the sound comes out of the speaker. */
    fun becameNoisy() {
        Log.i(TAG, "Headphones gone: pausing")
        pause()
    }

    // --- The queue ---------------------------------------------------------------------------

    /**
     * Starts playback: audio focus first (none during a phone call), then the service, so
     * Android lets it play on when the app goes to the background.
     */
    private fun play(name: String, done: () -> Unit, worked: () -> Unit = {}, call: (CoreQueue) -> Unit) {
        if (!requestFocus()) {
            told.value = "Another app is using the sound (a call?). Try again after it."
            done()
            return
        }
        keepGoing(name, done, worked, call)
        if (!PlaybackService.inForeground) {
            try {
                app.startForegroundService(Intent(app, PlaybackService::class.java).setAction(PlaybackService.START))
            } catch (e: Exception) {
                // Only from the background without a system button: it plays, but Android may stop it.
                Log.w(TAG, "could not start the playback service: $e")
            }
        }
    }

    /** A command after which playback goes on: the service stays in the foreground meanwhile. */
    private fun keepGoing(name: String, done: () -> Unit, worked: () -> Unit = {}, call: (CoreQueue) -> Unit) {
        starting++
        command(name, {
            starting--
            settle()
            changed()
            done()
        }, worked, call)
    }

    /**
     * Runs `call` on the queue off the main thread (making the queue first if needed). Then,
     * on the main thread: `worked` if it did, [message] says why if not, and `done` always.
     */
    private fun command(name: String, done: () -> Unit, worked: () -> Unit = {}, call: (CoreQueue) -> Unit) {
        commands.execute {
            val failed = try {
                call(queue())
                null
            } catch (e: Exception) {
                Log.w(TAG, "$name failed: ${describe(e)}")
                describe(e)
            }
            main.post {
                if (failed == null) {
                    told.value = null
                    worked()
                } else {
                    told.value = failed
                }
                done()
            }
        }
    }

    @Synchronized
    private fun queue(): CoreQueue = queue ?: CoreQueue().also { made ->
        made.setListener(object : QueueListener {
            override fun onState(state: QueueState) {
                main.post { publish(state) }
            }
        })
        queue = made
        target = made.videoTarget()
        // A video screen that came first gets its picture now.
        main.post { waitingSurface?.let { attachSurface(it) } }
    }

    // --- Video (VideoView) ----------------------------------------------------------------

    /**
     * A Surface to show video on (main thread; VideoView). The newest one wins: going full
     * screen makes a second view, whose Surface can come before the first one goes.
     */
    fun attachSurface(surface: Surface) {
        val number = target
        if (number == 0L) {
            waitingSurface = surface
            return
        }
        waitingSurface = null
        shownSurface = if (VideoSurface.attach(number, surface)) surface else null
        Log.i(TAG, if (shownSurface == null) "the video surface was not taken" else "video surface attached to $number")
    }

    fun resizeSurface(surface: Surface, width: Int, height: Int) {
        if (shownSurface === surface) VideoSurface.resize(target, width, height)
    }

    /**
     * A Surface goes: if it is the one shown, video off (the sound plays on). mpv is done
     * with it when this returns, as Android requires.
     */
    fun detachSurface(surface: Surface) {
        if (waitingSurface === surface) waitingSurface = null
        if (shownSurface === surface) {
            shownSurface = null
            VideoSurface.detach(target)
            Log.i(TAG, "video surface detached")
        }
    }

    private fun publish(state: QueueState) {
        val old = shown.value
        if (old != null && old.revision >= state.revision) return
        shown.value = state
        settle()
        changed()
    }

    /**
     * Done playing (the end, or nothing queued): the focus goes back to other apps. Not
     * while a play is on its way: the queue's first state is empty, and comes after the focus.
     */
    private fun settle() {
        val state = shown.value ?: return
        if (starting == 0 && !resumeAfterLoss && !state.wantsPlay && (state.finished || (state.entryCount == 0u))) {
            abandonFocus()
        }
    }

    private fun changed() {
        fetchEntries()
        listenForNoise(keepsGoing)
        watchers.forEach { it() }
        shown.value?.let { SelfTest.onQueue(it) }
    }

    // --- Audio focus -------------------------------------------------------------------------

    private val audio by lazy { app.getSystemService(AudioManager::class.java) }
    private val focus by lazy {
        AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build(),
            )
            .setOnAudioFocusChangeListener({ onFocusChange(it) }, main)
            .build()
    }
    private var hasFocus = false

    /** Paused by a call (not by the person): plays on when the focus comes back. */
    private var resumeAfterLoss = false
    private var ducked = false

    private fun requestFocus(): Boolean {
        if (hasFocus) return true
        hasFocus = audio.requestAudioFocus(focus) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        if (!hasFocus) {
            Log.w(TAG, "No audio focus (a call?): not playing")
            return false
        }
        // Lowered for a voice when the focus went away: back to full.
        if (ducked) volumeUp()
        return true
    }

    private fun abandonFocus() {
        if (!hasFocus) return
        audio.abandonAudioFocusRequest(focus)
        hasFocus = false
    }

    private fun volumeUp() {
        ducked = false
        command("volume up", {}) { it.setVolume(100.0) }
    }

    private fun onFocusChange(change: Int) {
        Log.i(TAG, "audio focus: $change")
        when (change) {
            AudioManager.AUDIOFOCUS_GAIN -> {
                hasFocus = true
                if (ducked) volumeUp()
                if (resumeAfterLoss) {
                    resumeAfterLoss = false
                    keepGoing("resume after a call", {}) { it.resume() }
                }
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> {
                ducked = true
                command("volume down", {}) { it.setVolume(DUCKED) }
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                // Set before pausing, so the service stays in the foreground through the call.
                if (shown.value?.wantsPlay == true || starting > 0) {
                    resumeAfterLoss = true
                    command("pause for a call", { changed() }) { it.pause() }
                }
            }
            AudioManager.AUDIOFOCUS_LOSS -> {
                // Another app plays now: pause for good, and give the focus back.
                abandonFocus()
                if (resumeAfterLoss) {
                    resumeAfterLoss = false
                    changed()
                }
                command("pause for another app", {}) { it.pause() }
            }
        }
    }

    // --- Headphones unplugged ----------------------------------------------------------------

    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) becameNoisy()
        }
    }
    private var listening = false

    private fun listenForNoise(on: Boolean) {
        if (on == listening) return
        if (on) {
            val filter = IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
            if (Build.VERSION.SDK_INT >= 33) {
                app.registerReceiver(noisy, filter, Context.RECEIVER_NOT_EXPORTED)
            } else {
                app.registerReceiver(noisy, filter)
            }
        } else {
            app.unregisterReceiver(noisy)
        }
        listening = on
    }
}
