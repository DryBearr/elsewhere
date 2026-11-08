package io.github.drybearr.elsewhere

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Home
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector

/** The app's tabs. Each screen is its own file; list it here to plug it in. */
private enum class Tab(val label: String, val icon: ImageVector) {
    Downloads("Downloads", Icons.AutoMirrored.Filled.List),
    Add("Add", Icons.Filled.Add),
    Library("Library", Icons.Filled.Home),
}

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (savedInstanceState == null) {
            SelfTest.run(applicationContext, intent?.getStringExtra("selftest"), intent?.getBooleanExtra(Health.RESTARTED, false) == true)
        }
        setContent {
            MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
                App()
            }
        }
    }
}

@Composable
private fun App() {
    var tab by rememberSaveable { mutableIntStateOf(Tab.Downloads.ordinal) }
    var openJob by rememberSaveable { mutableStateOf<Long?>(null) }
    Scaffold(bottomBar = {
        NavigationBar {
            Tab.entries.forEach { entry ->
                NavigationBarItem(
                    selected = tab == entry.ordinal,
                    onClick = {
                        tab = entry.ordinal
                        openJob = null
                    },
                    icon = { Icon(entry.icon, contentDescription = null) },
                    label = { Text(entry.label) },
                )
            }
        }
    }) { bars ->
        Box(Modifier.padding(bars).imePadding()) {
            val job = openJob
            when {
                job != null -> JobDetailScreen(job) { openJob = null }
                tab == Tab.Downloads.ordinal -> DownloadsScreen { openJob = it }
                tab == Tab.Add.ordinal -> AddScreen { tab = Tab.Downloads.ordinal }
                else -> LibraryScreen()
            }
        }
    }
}
