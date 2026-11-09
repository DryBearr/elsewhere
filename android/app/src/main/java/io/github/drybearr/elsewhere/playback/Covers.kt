package io.github.drybearr.elsewhere.playback

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Log
import kotlin.math.max

/** Cover images, scaled down as they load (a cover can be a 4000-pixel photo). */
object Covers {
    /** The image at `path` at most `size` pixels wide and tall, or null if it does not load. */
    fun load(path: String, size: Int): Bitmap? = try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(path, bounds)
        var sample = 1
        while (max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= size) sample *= 2
        val image = BitmapFactory.decodeFile(path, BitmapFactory.Options().apply { inSampleSize = sample })
        if (image == null) {
            null
        } else {
            val scale = size.toFloat() / max(image.width, image.height)
            if (scale >= 1f) {
                image
            } else {
                Bitmap.createScaledBitmap(
                    image, max(1, (image.width * scale).toInt()), max(1, (image.height * scale).toInt()), true,
                )
            }
        }
    } catch (e: Throwable) {
        // A broken or huge image: no cover rather than a crash.
        Log.w("Elsewhere", "cover did not load: $e")
        null
    }
}
