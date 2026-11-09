package io.github.drybearr.elsewhere.playback

import android.content.Context
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView

/**
 * The core's JNI functions for video (src/player/android.rs): mpv draws a video into a
 * Surface itself. Loading the library this way (it is loaded already, through JNA) also
 * gives the core the Java VM, which mpv needs to reach the Surface.
 */
object VideoSurface {
    init {
        System.loadLibrary("elsewhere")
    }

    /** Video on, drawn into `surface`, for the player numbered `target`. */
    @JvmStatic
    external fun attach(target: Long, surface: Surface): Boolean

    /** Video off; when it returns, the Surface is free again. */
    @JvmStatic
    external fun detach(target: Long)

    @JvmStatic
    external fun resize(target: Long, width: Int, height: Int)
}

/**
 * A SurfaceView that shows the playlist player's video while it is on screen. When it
 * goes (another screen, the app in the background) the video goes off and the sound
 * plays on.
 */
class VideoView(context: Context) : SurfaceView(context), SurfaceHolder.Callback {
    init {
        holder.addCallback(this)
        Playback.videoEvents.addLast("video view made")
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        Playback.videoEvents.addLast("video view on screen")
    }

    override fun onSizeChanged(width: Int, height: Int, oldWidth: Int, oldHeight: Int) {
        super.onSizeChanged(width, height, oldWidth, oldHeight)
        Playback.videoEvents.addLast("video view ${width}x$height")
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        Playback.videoEvents.addLast("surface created ${holder.surfaceFrame}")
        Playback.attachSurface(holder.surface)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        Playback.resizeSurface(holder.surface, width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // Must not return before mpv lets go of the Surface: detach waits for that.
        Playback.detachSurface(holder.surface)
    }
}
