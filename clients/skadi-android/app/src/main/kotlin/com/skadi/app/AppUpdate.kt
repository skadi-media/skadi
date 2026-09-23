package com.skadi.app

import kotlinx.coroutines.delay
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.Lifecycle
import androidx.compose.ui.platform.LocalLifecycleOwner
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import com.skadi.core.AppManifest
import com.skadi.core.SkadiApi
import kotlinx.coroutines.launch
import java.io.File

/**
 * In-app update (SKADI-T-0579).
 *
 * The daemon already publishes the APK it wants phones to run, at
 * `/app/manifest.json` + `/app/<file>` — that is what the desktop UI's install
 * QR points at. Until now the only way to move to a new build was to open that
 * page on the phone and reinstall by hand. Now the app reads the same manifest,
 * and when the offered `version_code` is above its own it says so on Home:
 * one tap downloads the APK and hands it to the system installer.
 *
 * What the app cannot do is skip Android's own gates: the first time, the user
 * has to allow Skadi as an install source (a Settings toggle this opens for
 * them); every time, the installer shows its confirm sheet. Both are the OS's,
 * not ours, and they are what makes sideloading safe enough to offer.
 *
 * The update installs over the running app, which works only because every
 * published build is signed with the same release key (`publish-apk.sh`).
 */
object UpdateCheck {
    @Volatile var offered: AppManifest? = null
    @Volatile var checkedAt = 0L
    /** Last failure, so a panel can say why nothing is offered. */
    @Volatile var lastError: String? = null
    /** Bump to force a re-check (the server panel's "Check for updates"). */
    val nonce = mutableStateOf(0)
    const val RECHECK_MS = 10 * 60 * 1000L
    const val RETRY_MS = 15 * 1000L
    const val RETRIES = 4
}

/**
 * Fetch the manifest and remember the answer. A failed fetch is retried a few
 * times (SKADI-T-0607): at cold start the request races the phone's VPN or
 * Wi-Fi coming back, and the one-shot check used to swallow that failure and
 * show nothing until the next tab switch — "there's no upgrade banner".
 */
suspend fun checkForUpdate(api: SkadiApi, force: Boolean): AppManifest? {
    if (!force && System.currentTimeMillis() - UpdateCheck.checkedAt < UpdateCheck.RECHECK_MS) {
        return UpdateCheck.offered
    }
    repeat(UpdateCheck.RETRIES) { attempt ->
        val r = runCatching { api.appManifest() }
        r.onSuccess {
            UpdateCheck.offered = it
            UpdateCheck.checkedAt = System.currentTimeMillis()
            UpdateCheck.lastError = null
            return it
        }
        UpdateCheck.lastError = r.exceptionOrNull()?.message ?: "manifest fetch failed"
        if (attempt < UpdateCheck.RETRIES - 1) delay(UpdateCheck.RETRY_MS)
    }
    return UpdateCheck.offered
}

@Composable
fun UpdateCard(api: SkadiApi) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val owner = LocalLifecycleOwner.current
    var offered by remember { mutableStateOf(UpdateCheck.offered) }
    var progress by remember { mutableStateOf<Float?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    val nonce by UpdateCheck.nonce
    // Re-check whenever the screen comes back to the foreground (not only on
    // first composition), and whenever the nonce is bumped by hand.
    LaunchedEffect(nonce, owner) {
        owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            offered = checkForUpdate(api, force = nonce > 0)
        }
    }
    val m = offered?.takeIf { it.versionCode > BuildConfig.VERSION_CODE } ?: return

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    "Skadi ${m.versionName} is available",
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Text(
                    error ?: "You have ${BuildConfig.VERSION_NAME}. Android will ask you to confirm the install.",
                    style = MaterialTheme.typography.bodySmall,
                    color = if (error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Button(
                enabled = progress == null,
                onClick = {
                    error = null
                    // API 26+: the OS refuses to install from an app the user
                    // has not allowed as a source. Open the exact Settings page
                    // for this app; the next tap here goes straight through.
                    if (Build.VERSION.SDK_INT >= 26 && !context.packageManager.canRequestPackageInstalls()) {
                        context.startActivity(
                            Intent(
                                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                Uri.parse("package:${context.packageName}"),
                            ),
                        )
                        return@Button
                    }
                    scope.launch {
                        progress = 0f
                        runCatching {
                            val file = File(context.cacheDir, "updates/${m.file}")
                            api.downloadTo(api.apkUrl(m.file), file) { progress = it }
                            install(context, file)
                        }.onFailure { error = it.message ?: "Update failed" }
                        progress = null
                    }
                },
                modifier = Modifier.padding(start = 12.dp),
            ) { Text(if (progress == null) "Update" else "Downloading…") }
        }
        progress?.let { p ->
            LinearProgressIndicator(
                progress = { if (p < 0f) 0f else p },
                trackColor = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp).height(4.dp),
            )
        }
    }
}

private fun install(context: android.content.Context, apk: File) {
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", apk)
    context.startActivity(
        Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
        },
    )
}
