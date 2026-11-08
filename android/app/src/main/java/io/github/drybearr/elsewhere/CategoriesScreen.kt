package io.github.drybearr.elsewhere

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.elsewhere.categories

/** The library's categories, from the database in the Rust core. */
@Composable
fun CategoriesScreen() {
    val context = LocalContext.current
    val names by produceState(initialValue = emptyList<String>()) {
        value = withContext(Dispatchers.IO) {
            runCatching { categories(File(context.filesDir, "elsewhere.db").path) }.getOrDefault(emptyList())
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text("Categories", style = MaterialTheme.typography.titleMedium)
        names.forEach { Text(it) }
    }
}
