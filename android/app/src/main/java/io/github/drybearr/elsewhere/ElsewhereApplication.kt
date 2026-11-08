package io.github.drybearr.elsewhere

import android.app.Application

/** Starts the Rust core once, before any screen: it must be first (see Core.start). */
class ElsewhereApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Core.start(this)
    }
}
