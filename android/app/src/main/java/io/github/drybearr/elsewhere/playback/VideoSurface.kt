package io.github.drybearr.elsewhere.playback

import android.content.Context
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView

/**
 * The core's two JNI functions for video (src/player/android.rs): a Surface becomes a JNI
 * global reference, which mpv draws into (CoreQueue.attachSurface), and is let go after.
 * Loading the library this way also gives the core the Java VM, which mpv needs to reach
 * the Surface.
 */
object VideoSurface {
    init {
        System.loadLibrary("elsewhere")
    }

    /** A reference to `surface` for mpv; 0 if none could be made. */
    @JvmStatic
    external fun window(surface: Surface): Long

    /** Lets go of a reference from [window], once mpv no longer draws into it. */
    @JvmStatic
    external fun release(window: Long)
}

/**
 * A SurfaceView that shows the playlist player's video while it is on screen. When it
 * goes (another screen, the app in the background) the video goes off and the sound
 * plays on.
 */
class VideoView(context: Context, private val status: (String) -> Unit = {}) :
    SurfaceView(context), SurfaceHolder.Callback {
    init {
        holder.addCallback(this)
        status("made")
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        status("on screen")
    }

    override fun onSizeChanged(width: Int, height: Int, oldWidth: Int, oldHeight: Int) {
        super.onSizeChanged(width, height, oldWidth, oldHeight)
        status("${width}x$height")
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        status("surface created")
        status(Playback.attachSurface(holder.surface))
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        Playback.resizeSurface(holder.surface, width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        // Must not return before mpv lets go of the Surface: detach waits for that.
        Playback.detachSurface(holder.surface)
    }
}
