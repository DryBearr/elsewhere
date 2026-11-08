package io.github.drybearr.elsewhere

import android.app.Activity
import android.app.Application
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.util.Log
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.elsewhere.PartState
import uniffi.elsewhere.coreHealth
import kotlin.system.exitProcess

/**
 * The last resort. The core heals what it can by itself; a part it could not heal is
 * Broken, and then only a fresh process helps. This restarts the app: everything that
 * matters is in the database, so nothing is lost, and jobs that were running come back
 * Interrupted with Resume. Checks every 30 s and right after a job fails with a bug.
 *
 * In the foreground the app comes back at once; in the background it just ends, and the
 * next launch is fresh. At most one restart per 10 minutes, so a part that breaks again
 * right away cannot make the app loop.
 */
object Health {
    private const val TAG = "Elsewhere"
    private const val CHECK_MS = 30_000L
    private const val MIN_GAP_MS = 10 * 60_000L
    const val RESTARTED = "restarted"

    private lateinit var app: Application

    /** Activities started and not stopped: the app is on screen while above 0. */
    @Volatile
    private var visible = 0

    val isVisible: Boolean
        get() = visible > 0

    fun watch(application: Application) {
        app = application
        application.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
            override fun onActivityStarted(activity: Activity) { visible++ }
            override fun onActivityStopped(activity: Activity) { visible-- }
            override fun onActivityCreated(activity: Activity, state: Bundle?) {}
            override fun onActivityResumed(activity: Activity) {}
            override fun onActivityPaused(activity: Activity) {}
            override fun onActivitySaveInstanceState(activity: Activity, state: Bundle) {}
            override fun onActivityDestroyed(activity: Activity) {}
        })
        Core.io.launch {
            while (true) {
                delay(CHECK_MS)
                checkNow(force = false)
            }
        }
    }

    /** Checks soon, off the calling thread (the core's delivery thread calls this). */
    fun check() {
        if (::app.isInitialized) Core.io.launch { checkNow(force = false) }
    }

    /** `force` skips the 10 minute gap (the self-test restarts on purpose). */
    fun checkNow(force: Boolean) {
        if (!::app.isInitialized) return
        val health = runCatching { coreHealth() }.getOrNull() ?: return
        if (!health.needsRestart) return
        val broken = health.parts.filter { it.state == PartState.BROKEN }
            .joinToString { "${it.name}: ${it.reason}" }
        val prefs = app.getSharedPreferences("health", Context.MODE_PRIVATE)
        val now = System.currentTimeMillis()
        if (!force && now - prefs.getLong("restarted_at", 0) < MIN_GAP_MS) {
            Log.w(TAG, "the core needs a restart ($broken), but it restarted less than 10 minutes ago")
            return
        }
        prefs.edit().putLong("restarted_at", now).commit()
        Log.w(TAG, "the core needs a restart: $broken")
        val launch = app.packageManager.getLaunchIntentForPackage(app.packageName)?.component
        if (visible > 0 && launch != null) {
            app.startActivity(Intent.makeRestartActivityTask(launch).putExtra(RESTARTED, true))
        }
        exitProcess(0)
    }
}
