package com.skadi.app

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

/**
 * The app's settings (SKADI-T-0721), from the gear on every tab's top bar.
 * Choices of this phone, stored in [Settings]: they need no server, so the
 * screen works offline too. One row now; more go below it as sections.
 */
@Composable
fun SettingsScreen(onBack: () -> Unit) {
    Column(modifier = Modifier.fillMaxSize()) {
        SkadiTopBar(title = "Settings", onBack = onBack)
        Column(modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
            Section("Appearance")
            ThemeRow()
        }
    }
}

@Composable
private fun Section(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 14.dp, bottom = 4.dp),
    )
}

/** System, Light, Dark or Hyper. A tap saves at once and MainActivity redraws. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ThemeRow() {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val current by Settings.theme(context).collectAsState(initial = null)
    Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text("Theme", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth().padding(top = 6.dp)) {
            val modes = SkadiThemeMode.entries
            modes.forEachIndexed { i, mode ->
                SegmentedButton(
                    selected = mode == current,
                    onClick = { scope.launch { Settings.saveTheme(context.applicationContext, mode) } },
                    shape = SegmentedButtonDefaults.itemShape(index = i, count = modes.size),
                ) { Text(mode.label, maxLines = 1) }
            }
        }
    }
}
