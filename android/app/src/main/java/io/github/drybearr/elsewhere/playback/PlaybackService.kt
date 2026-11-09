package io.github.drybearr.elsewhere.playback

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.drawable.Icon
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import kotlin.math.abs
import uniffi.elsewhere.PlayerStatus
import uniffi.elsewhere.QueueState

/**
 * Keeps playback going in the background: a media session (lock screen, Bluetooth and
 * headphone buttons, the system's media controls) and the notification that comes with it.
 * It shows what Playback's state says and sends the buttons to Playback.
 *
 * While playback goes on (or waits for a call to end) the service runs in the foreground,
 * so Android lets it play; paused, the notification stays and can be swiped away (which
 * stops playback); with nothing queued the service stops.
 */
class PlaybackService : Service() {
    companion object {
        private const val TAG = "Elsewhere"

        /** What Playback starts the service with: it must go to the foreground at once. */
        const val START = "io.github.drybearr.elsewhere.playback.START"
        private const val PLAY = "io.github.drybearr.elsewhere.playback.PLAY"
        private const val PAUSE = "io.github.drybearr.elsewhere.playback.PAUSE"
        private const val NEXT = "io.github.drybearr.elsewhere.playback.NEXT"
        private const val PREVIOUS = "io.github.drybearr.elsewhere.playback.PREVIOUS"
        private const val DISMISS = "io.github.drybearr.elsewhere.playback.DISMISS"

        private const val CHANNEL = "playback"
        private const val NOTIFICATION = 1

        /** Covers are scaled to this many pixels at most (a notification must stay small). */
        private const val COVER = 320

        /** Whether the service runs in the foreground now (main thread only). */
        var inForeground = false
            private set
    }

    private val main = Handler(Looper.getMainLooper())
    private lateinit var session: MediaSession
    private lateinit var notifications: NotificationManager
    private val watcher = { render() }
    private var gone = false

    private var shownMetadata: List<Any?>? = null
    private var shownPlayback: PlaybackState? = null
    private var shownNotification: List<Any?>? = null
    private var coverPath: String? = null
    private var cover: Bitmap? = null

    /** Android refused the foreground for what is shown: no trying again until it changes. */
    private var refused = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        notifications = getSystemService(NotificationManager::class.java)
        notifications.createNotificationChannel(
            NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW).apply {
                setShowBadge(false)
            },
        )
        // Opens the app the way the launcher does (its task, not a second copy).
        val open = packageManager.getLaunchIntentForPackage(packageName)!!
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
        session = MediaSession(this, "Elsewhere").apply {
            setCallback(buttons, main)
            setSessionActivity(PendingIntent.getActivity(this@PlaybackService, 0, open, PendingIntent.FLAG_IMMUTABLE))
        }
        Playback.watch(watcher)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            // Started with startForegroundService: Android stops the app unless it goes
            // to the foreground now, whatever the state says.
            START -> goForeground(notification(Playback.state.value, true))
            PLAY -> Playback.resume()
            PAUSE -> Playback.pause()
            NEXT -> Playback.next()
            PREVIOUS -> Playback.previous()
            DISMISS -> Playback.stop()
        }
        render()
        return START_NOT_STICKY
    }

    /** The app was swiped away: playback goes on if it plays, else everything ends. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        if (!Playback.keepsGoing) end()
    }

    override fun onDestroy() {
        gone = true
        Playback.unwatch(watcher)
        if (inForeground) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            inForeground = false
        }
        notifications.cancel(NOTIFICATION)
        session.release()
        super.onDestroy()
    }

    /** The system's buttons (lock screen, notification on Android 13+, Bluetooth, headphones). */
    private val buttons = object : MediaSession.Callback() {
        override fun onPlay() = Playback.resume()
        override fun onPause() = Playback.pause()
        override fun onSkipToNext() = Playback.next()
        override fun onSkipToPrevious() = Playback.previous()
        override fun onSeekTo(pos: Long) = Playback.seek(pos)
        override fun onStop() = Playback.stop()
    }

    // --- Showing the state -------------------------------------------------------------------

    private fun render() {
        if (gone) return
        val state = Playback.state.value
        val index = state?.current?.toInt()
        if (state == null || index == null || index.toUInt() >= state.entryCount) {
            // Nothing queued (stopped, or a play that failed): end, unless a play is on its way.
            if (!Playback.keepsGoing) end()
            return
        }
        if (!session.isActive) session.isActive = true
        showCover(state.nowPlaying?.cover)
        showMetadata(state)
        showPlayback(state)

        val going = Playback.keepsGoing
        val key = listOf(state.nowPlaying, state.wantsPlay, state.hasNext, going, cover)
        if (key == shownNotification && (going == inForeground || refused)) return
        if (key != shownNotification) refused = false
        shownNotification = key
        val notification = notification(state, going)
        when {
            going && !inForeground -> goForeground(notification)
            !going && inForeground -> {
                // Paused: the notification stays, and can be swiped away now.
                stopForeground(STOP_FOREGROUND_DETACH)
                inForeground = false
                notifications.notify(NOTIFICATION, notification)
            }
            else -> notifications.notify(NOTIFICATION, notification)
        }
    }

    private fun goForeground(notification: Notification) {
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
            } else {
                startForeground(NOTIFICATION, notification)
            }
            inForeground = true
        } catch (e: Exception) {
            refused = true
            // From the background without a system button (Android 12+): it plays on, but
            // Android may stop it.
            Log.w(TAG, "could not go to the foreground: $e")
        }
    }

    /** Nothing to play: the session, the notification and the service go. */
    private fun end() {
        if (gone) return
        gone = true
        session.isActive = false
        if (inForeground) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            inForeground = false
        }
        notifications.cancel(NOTIFICATION)
        stopSelf()
    }

    private fun showMetadata(state: QueueState) {
        val now = state.nowPlaying
        val duration = state.playback.durationMs ?: -1
        val key = listOf(now, duration, cover)
        if (key == shownMetadata) return
        shownMetadata = key
        val metadata = MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, now?.title ?: "")
            .putString(MediaMetadata.METADATA_KEY_ARTIST, now?.artist)
            .putString(MediaMetadata.METADATA_KEY_ALBUM, now?.album)
            .putLong(MediaMetadata.METADATA_KEY_DURATION, duration)
        cover?.let { metadata.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it) }
        session.setMetadata(metadata.build())
    }

    private fun showPlayback(state: QueueState) {
        val playback = state.playback
        val code = when {
            // The end came because the last entry could not be played.
            state.finished && playback.status == PlayerStatus.FAILED -> PlaybackState.STATE_ERROR
            state.finished -> PlaybackState.STATE_STOPPED
            playback.status == PlayerStatus.PLAYING && state.wantsPlay -> PlaybackState.STATE_PLAYING
            state.wantsPlay -> PlaybackState.STATE_BUFFERING
            else -> PlaybackState.STATE_PAUSED
        }
        // The system moves the position on by itself while the speed is not 0.
        val speed = if (code == PlaybackState.STATE_PLAYING) playback.speed.toFloat() else 0f
        var actions = PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or
            PlaybackState.ACTION_PLAY_PAUSE or PlaybackState.ACTION_STOP or
            PlaybackState.ACTION_SKIP_TO_PREVIOUS
        if (state.hasNext) actions = actions or PlaybackState.ACTION_SKIP_TO_NEXT
        if ((playback.durationMs ?: 0) > 0) actions = actions or PlaybackState.ACTION_SEEK_TO
        val error = if (code == PlaybackState.STATE_ERROR) state.problem?.text else null
        val now = SystemClock.elapsedRealtime()

        // Positions come 4 times a second; tell the system only when it would be wrong.
        val old = shownPlayback
        if (old != null && old.state == code && old.actions == actions &&
            old.playbackSpeed == speed && old.errorMessage?.toString() == error
        ) {
            val expected = old.position + ((now - old.lastPositionUpdateTime) * old.playbackSpeed).toLong()
            val off = abs(expected - playback.positionMs)
            if (if (speed == 0f) off == 0L else off < 1000) return
        }
        val shown = PlaybackState.Builder()
            .setState(code, playback.positionMs, speed, now)
            .setActions(actions)
            .apply { if (error != null) setErrorMessage(error) }
            .build()
        shownPlayback = shown
        session.setPlaybackState(shown)
    }

    private fun notification(state: QueueState?, going: Boolean): Notification {
        val now = state?.nowPlaying
        val builder = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(now?.title ?: "Elsewhere")
            .setContentText(now?.artist)
            .setLargeIcon(cover)
            .setContentIntent(session.controller.sessionActivity)
            .setDeleteIntent(action(DISMISS))
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setCategory(Notification.CATEGORY_TRANSPORT)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setOngoing(going)
        // Android 13+ draws its own buttons from the session; older ones use these.
        val compact = mutableListOf<Int>()
        if (state?.current != null) {
            builder.addAction(button(android.R.drawable.ic_media_previous, "Previous", PREVIOUS))
            compact += 0
            if (state.wantsPlay) {
                builder.addAction(button(android.R.drawable.ic_media_pause, "Pause", PAUSE))
            } else {
                builder.addAction(button(android.R.drawable.ic_media_play, "Play", PLAY))
            }
            compact += 1
            if (state.hasNext) {
                builder.addAction(button(android.R.drawable.ic_media_next, "Next", NEXT))
                compact += 2
            }
        }
        builder.setStyle(
            Notification.MediaStyle()
                .setMediaSession(session.sessionToken)
                .setShowActionsInCompactView(*compact.toIntArray()),
        )
        return builder.build()
    }

    private fun button(icon: Int, title: String, name: String) =
        Notification.Action.Builder(Icon.createWithResource(this, icon), title, action(name)).build()

    private fun action(name: String): PendingIntent = PendingIntent.getService(
        this, name.hashCode(), Intent(this, PlaybackService::class.java).setAction(name),
        PendingIntent.FLAG_IMMUTABLE,
    )

    // --- The cover ---------------------------------------------------------------------------

    /** Loads the current entry's cover off the main thread; shows it once loaded. */
    private fun showCover(path: String?) {
        if (path == coverPath) return
        coverPath = path
        cover = null
        if (path == null) return
        Thread({
            val loaded = Covers.load(path, COVER)
            main.post {
                if (coverPath == path && loaded != null) {
                    cover = loaded
                    render()
                }
            }
        }, "elsewhere-cover").start()
    }
}
